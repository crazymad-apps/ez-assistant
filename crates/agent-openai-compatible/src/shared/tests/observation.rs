use super::*;

#[tokio::test]
async fn request_carries_url_method_auth_header_and_encoded_streaming_body() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        OK_SSE_BODY,
    ))]));
    let service = service_with(&transport);
    let request = simple_request();
    let stream = service
        .stream(request.clone(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    collected.assert_finished();

    let requests = transport.take_requests();
    assert_eq!(requests.len(), 1);
    let recorded = &requests[0];
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.url, format!("{BASE_URL}/chat/completions"));
    let expected_auth = format!("Bearer {TOKEN}");
    assert_eq!(
        recorded.header("authorization"),
        Some(expected_auth.as_str())
    );
    assert_eq!(recorded.header("content-type"), Some("application/json"));
    assert_eq!(recorded.header("accept"), Some("text/event-stream"));

    // body 就是 encode_request 的序列化产物，且固定 stream:true + include_usage。
    let expected_body = serde_json::to_vec(
        &encode_request(&request, &base_adapter(), "gpt-test").expect("encode succeeds"),
    )
    .expect("serialize encoded request");
    assert_eq!(recorded.body, expected_body);
    let chat_request: ChatRequest =
        serde_json::from_slice(&recorded.body).expect("body is a chat request");
    assert!(chat_request.stream);
    assert_eq!(
        chat_request.stream_options,
        Some(ChatStreamOptions {
            include_usage: true
        })
    );
}

#[tokio::test]
async fn request_carries_trace_only_as_transport_metadata() {
    let transport = Arc::new(TraceCaptureTransport {
        trace: Mutex::new(None),
    });
    let service = OpenAiChatCompletionsService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_adapter(),
        transport.clone(),
    )
    .expect("test base URL should be valid");
    let trace = TraceContext::new("logical-call-1")
        .with_attempt(NonZeroU32::new(2).expect("attempt should be non-zero"));
    let stream = service
        .stream(
            simple_request(),
            ModelCallContext::default().with_trace(trace.clone()),
        )
        .await
        .expect("stream established");
    EventCollector::collect_validated(stream)
        .await
        .assert_finished();

    assert_eq!(
        *transport
            .trace
            .lock()
            .expect("trace lock should not be poisoned"),
        Some(trace)
    );
}

#[test]
fn wire_bytes_use_lossless_base64_serde() {
    let bodies = [
        Vec::new(),
        "你好，wire".as_bytes().to_vec(),
        vec![0, 1, 2, 127, 128, 254, 255],
        vec![0xa5; 64 * 1024],
    ];

    for bytes in bodies {
        let request = RecordedWireRequest {
            method: "POST".to_owned(),
            url: format!("{BASE_URL}/chat/completions"),
            headers: Vec::new(),
            body: bytes.clone(),
        };
        let json = serde_json::to_value(&request).expect("wire request should serialize");
        assert!(json["body"].is_string());
        assert_eq!(
            serde_json::from_value::<RecordedWireRequest>(json)
                .expect("wire request should deserialize"),
            request
        );

        let event = ProviderWireEvent::ResponseChunk { trace: None, bytes };
        let json = serde_json::to_value(&event).expect("wire event should serialize");
        assert!(json["ResponseChunk"]["bytes"].is_string());
        assert_eq!(
            serde_json::from_value::<ProviderWireEvent>(json)
                .expect("wire event should deserialize"),
            event
        );
    }
}

#[test]
fn every_wire_event_variant_round_trips_json() {
    let trace = Some(TraceContext::new("call-1"));
    let events = [
        ProviderWireEvent::Request {
            trace: trace.clone(),
            request: RecordedWireRequest::from_transport_request(&wire_request(trace.clone())),
        },
        ProviderWireEvent::ResponseStarted {
            trace: trace.clone(),
            status: 429,
            headers: vec![("retry-after".to_owned(), "2".to_owned())],
        },
        ProviderWireEvent::ResponseChunk {
            trace: trace.clone(),
            bytes: vec![0, 255],
        },
        ProviderWireEvent::ResponseFailed {
            trace: trace.clone(),
            error: TransportError::Interrupted("connection reset".to_owned()),
        },
        ProviderWireEvent::ResponseFinished { trace },
    ];

    for event in events {
        let json = serde_json::to_vec(&event).expect("wire event should serialize");
        let decoded: ProviderWireEvent =
            serde_json::from_slice(&json).expect("wire event should deserialize");
        assert_eq!(decoded, event);
    }
}

#[test]
fn recorded_request_permanently_excludes_sensitive_headers() {
    let mut request = wire_request(None);
    request.headers.extend([
        ("Proxy-Authorization".to_owned(), "proxy-secret".to_owned()),
        ("X-API-Key".to_owned(), "api-secret".to_owned()),
        ("Cookie".to_owned(), "session=secret".to_owned()),
        ("Set-Cookie".to_owned(), "session=secret".to_owned()),
        ("x-custom".to_owned(), "kept".to_owned()),
    ]);

    let recorded = RecordedWireRequest::from_transport_request(&request);
    assert_eq!(
        recorded.headers,
        vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("x-custom".to_owned(), "kept".to_owned()),
        ]
    );
    assert_eq!(recorded.body, request.body);
    let json = serde_json::to_string(&recorded).expect("wire request should serialize");
    for secret in [TOKEN, "proxy-secret", "api-secret", "session=secret"] {
        assert!(!json.contains(secret));
    }
}

#[test]
fn service_rejects_base_url_userinfo_query_and_fragment_without_echoing_input() {
    for (base_url, expected_rule, secret) in [
        (
            "https://user:url-secret@api.openai.test/v1",
            "userinfo",
            "url-secret",
        ),
        (
            "https://api.openai.test/v1?api_key=query-secret",
            "query",
            "query-secret",
        ),
        (
            "https://api.openai.test/v1#fragment-secret",
            "fragment",
            "fragment-secret",
        ),
    ] {
        let error = OpenAiChatCompletionsService::with_transport(
            base_url,
            BearerCredential::new(TOKEN),
            "gpt-test",
            128_000,
            base_adapter(),
            Arc::new(RecordedTransport::new([])),
        )
        .err()
        .expect("unsafe base URL must be rejected");
        let message = error.to_string();
        assert!(message.contains(expected_rule));
        assert!(!message.contains(secret));
    }
}

#[tokio::test]
async fn observed_transport_preserves_request_response_headers_and_chunk_boundaries() {
    let response_headers = vec![
        ("Content-Type".to_owned(), "text/event-stream".to_owned()),
        ("Retry-After".to_owned(), "2".to_owned()),
        ("Request-ID".to_owned(), "request-1".to_owned()),
        ("X-Request-ID".to_owned(), "request-2".to_owned()),
        ("RateLimit-Limit".to_owned(), "100".to_owned()),
        ("X-RateLimit-Remaining".to_owned(), "99".to_owned()),
        ("Set-Cookie".to_owned(), "secret=value".to_owned()),
        ("Server".to_owned(), "private-upstream".to_owned()),
    ];
    let request = wire_request(Some(TraceContext::new("call-headers")));
    let inner = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![
            BodyStep::Chunk(vec![0, 1, 2]),
            BodyStep::Chunk(vec![255, 3]),
        ],
    )
    .with_header("Content-Type", "text/event-stream")
    .with_header("Retry-After", "2")
    .with_header("Request-ID", "request-1")
    .with_header("X-Request-ID", "request-2")
    .with_header("RateLimit-Limit", "100")
    .with_header("X-RateLimit-Remaining", "99")
    .with_header("Set-Cookie", "secret=value")
    .with_header("Server", "private-upstream"))]));
    let observer = Arc::new(WireCollector::default());
    let transport = ObservedTransport::new(inner.clone(), observer.clone());
    let response = transport
        .execute(request.clone())
        .await
        .expect("response should establish");
    assert_eq!(response.headers, response_headers);
    let items: Vec<_> = response.body.collect().await;
    assert_eq!(items, vec![Ok(vec![0, 1, 2]), Ok(vec![255, 3])]);

    let lower_requests = inner.take_requests();
    assert_eq!(lower_requests.len(), 1);
    assert_eq!(lower_requests[0].method, request.method);
    assert_eq!(lower_requests[0].url, request.url);
    assert_eq!(lower_requests[0].headers, request.headers);
    assert_eq!(lower_requests[0].body, request.body);

    let events = observer.events();
    let ProviderWireEvent::Request {
        request: recorded, ..
    } = &events[0]
    else {
        panic!("first wire event must be the request");
    };
    assert!(
        recorded
            .headers
            .iter()
            .all(|(name, _)| !name.eq_ignore_ascii_case("authorization"))
    );
    assert_eq!(recorded.body, request.body);
    assert_eq!(
        events[1],
        ProviderWireEvent::ResponseStarted {
            trace: Some(TraceContext::new("call-headers")),
            status: 200,
            headers: response_headers[..6].to_vec(),
        }
    );
    let recorded_headers_json =
        serde_json::to_string(&events[1]).expect("response event should serialize");
    assert!(!recorded_headers_json.contains("secret=value"));
    assert!(!recorded_headers_json.contains("private-upstream"));
    assert!(matches!(
        events.last(),
        Some(ProviderWireEvent::ResponseFinished { .. })
    ));
}

#[tokio::test]
async fn observed_transport_preserves_stream_failure_position_without_finished_event() {
    let inner = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![
            BodyStep::Chunk(b"first".to_vec()),
            BodyStep::Fail("connection reset".to_owned()),
            BodyStep::Chunk(b"unreachable".to_vec()),
        ],
    ))]));
    let observer = Arc::new(WireCollector::default());
    let transport = ObservedTransport::new(inner, observer.clone());

    let response = transport
        .execute(wire_request(None))
        .await
        .expect("response should establish");
    let items: Vec<_> = response.body.collect().await;
    assert_eq!(
        items,
        vec![
            Ok(b"first".to_vec()),
            Err(TransportError::Interrupted("connection reset".to_owned())),
        ]
    );
    let events = observer.events();
    assert!(matches!(events[2], ProviderWireEvent::ResponseChunk { .. }));
    assert!(matches!(
        events[3],
        ProviderWireEvent::ResponseFailed {
            error: TransportError::Interrupted(_),
            ..
        }
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderWireEvent::ResponseFinished { .. }))
    );
}

#[tokio::test]
async fn observed_transport_reports_establishment_failure_unchanged() {
    let expected = TransportError::Connect("connection refused".to_owned());
    let inner = Arc::new(StubTransport {
        error: expected.clone(),
    });
    let observer = Arc::new(WireCollector::default());
    let transport = ObservedTransport::new(inner, observer.clone());

    let error = transport
        .execute(wire_request(None))
        .await
        .err()
        .expect("request should fail before response");
    assert_eq!(error, expected);
    assert!(matches!(
        observer.events().as_slice(),
        [ProviderWireEvent::Request { .. }, ProviderWireEvent::ResponseFailed { error, .. }]
            if error == &expected
    ));
}

#[tokio::test]
async fn observer_dropping_every_event_cannot_change_transport_result() {
    let expected = TransportError::Timeout;
    let transport = ObservedTransport::new(
        Arc::new(StubTransport {
            error: expected.clone(),
        }),
        Arc::new(DroppingObserver),
    );
    let actual = transport
        .execute(wire_request(None))
        .await
        .err()
        .expect("request should fail");
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn dropping_response_body_does_not_drain_or_emit_finished() {
    let inner = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![
            BodyStep::Chunk(b"first".to_vec()),
            BodyStep::Chunk(b"second".to_vec()),
        ],
    ))]));
    let observer = Arc::new(WireCollector::default());
    let transport = ObservedTransport::new(inner, observer.clone());
    let mut response = transport
        .execute(wire_request(None))
        .await
        .expect("response should establish");

    assert_eq!(response.body.next().await, Some(Ok(b"first".to_vec())));
    drop(response);
    let events = observer.events();
    assert_eq!(events.len(), 3);
    assert!(matches!(events[2], ProviderWireEvent::ResponseChunk { .. }));
}

#[tokio::test]
async fn cancellation_while_waiting_for_headers_stops_observation_with_the_call() {
    let entered = Arc::new(Notify::new());
    let observer = Arc::new(WireCollector::default());
    let transport = Arc::new(ObservedTransport::new(
        Arc::new(HangingTransport {
            entered: entered.clone(),
        }),
        observer.clone(),
    ));
    let service = Arc::new(
        OpenAiChatCompletionsService::with_transport(
            BASE_URL,
            BearerCredential::new(TOKEN),
            "gpt-test",
            128_000,
            base_adapter(),
            transport,
        )
        .expect("test base URL should be valid"),
    );
    let cancellation = CancellationToken::new();
    let task = {
        let service = service.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            service
                .stream(simple_request(), ModelCallContext::new(cancellation))
                .await
        })
    };
    entered.notified().await;
    cancellation.cancel();

    let error = task
        .await
        .expect("model task should join")
        .err()
        .expect("model call should be cancelled");
    assert_eq!(error, ModelError::Cancelled);
    assert!(matches!(
        observer.events().as_slice(),
        [ProviderWireEvent::Request { .. }]
    ));
}

#[test]
fn repeated_encoding_is_byte_for_byte_deterministic() {
    let request = simple_request();
    let first = serde_json::to_vec(
        &encode_request(&request, &base_adapter(), "gpt-test").expect("encode should succeed"),
    )
    .expect("encoded request should serialize");
    let second = serde_json::to_vec(
        &encode_request(&request, &base_adapter(), "gpt-test").expect("encode should succeed"),
    )
    .expect("encoded request should serialize");
    assert_eq!(first, second);
}

#[tokio::test]
async fn observed_and_plain_transports_produce_identical_model_events_and_http_requests() {
    let plain_inner = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        OK_SSE_BODY,
    ))]));
    let observed_inner = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        OK_SSE_BODY,
    ))]));
    let observer = Arc::new(WireCollector::default());
    let plain_service = service_with(&plain_inner);
    let observed_service = OpenAiChatCompletionsService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_adapter(),
        Arc::new(ObservedTransport::new(
            observed_inner.clone(),
            observer.clone(),
        )),
    )
    .expect("test base URL should be valid");

    let plain_stream = plain_service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .expect("plain stream should establish");
    let observed_stream = observed_service
        .stream(
            simple_request(),
            ModelCallContext::default().with_trace(TraceContext::new("observed-call")),
        )
        .await
        .expect("observed stream should establish");
    let (plain, observed) = tokio::join!(
        EventCollector::collect_validated(plain_stream),
        EventCollector::collect_validated(observed_stream)
    );

    assert_eq!(plain.events(), observed.events());
    assert_eq!(plain_inner.take_requests(), observed_inner.take_requests());
    assert!(matches!(
        observer.events().as_slice(),
        [
            ProviderWireEvent::Request { .. },
            ProviderWireEvent::ResponseStarted { .. },
            ProviderWireEvent::ResponseChunk { .. }
        ]
    ));
}

#[tokio::test]
async fn concurrent_calls_keep_correlation_and_attempt_on_each_wire_event() {
    let inner = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(200, OK_SSE_BODY)),
        Ok(RecordedResponse::new(200, OK_SSE_BODY)),
    ]));
    let observer = Arc::new(WireCollector::default());
    let service = OpenAiChatCompletionsService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_adapter(),
        Arc::new(ObservedTransport::new(inner, observer.clone())),
    )
    .expect("test base URL should be valid");
    let first_trace = TraceContext::new("logical-call-a")
        .with_attempt(NonZeroU32::new(1).expect("attempt should be non-zero"));
    let second_trace = TraceContext::new("logical-call-b")
        .with_attempt(NonZeroU32::new(2).expect("attempt should be non-zero"));

    let (first, second) = tokio::join!(
        service.stream(
            simple_request(),
            ModelCallContext::default().with_trace(first_trace.clone())
        ),
        service.stream(
            simple_request(),
            ModelCallContext::default().with_trace(second_trace.clone())
        )
    );
    let (first, second) = tokio::join!(
        EventCollector::collect_validated(first.expect("first stream should establish")),
        EventCollector::collect_validated(second.expect("second stream should establish"))
    );
    first.assert_finished();
    second.assert_finished();

    let events = observer.events();
    for expected in [&first_trace, &second_trace] {
        let matching: Vec<_> = events
            .iter()
            .filter(|event| match event {
                ProviderWireEvent::Request { trace, .. }
                | ProviderWireEvent::ResponseStarted { trace, .. }
                | ProviderWireEvent::ResponseChunk { trace, .. }
                | ProviderWireEvent::ResponseFailed { trace, .. }
                | ProviderWireEvent::ResponseFinished { trace } => trace.as_ref() == Some(expected),
            })
            .collect();
        assert_eq!(matching.len(), 3);
        assert!(matches!(matching[0], ProviderWireEvent::Request { .. }));
        assert!(matches!(
            matching[1],
            ProviderWireEvent::ResponseStarted { .. }
        ));
        assert!(matches!(
            matching[2],
            ProviderWireEvent::ResponseChunk { .. }
        ));
    }
}
