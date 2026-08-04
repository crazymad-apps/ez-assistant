//! Transport 边界与 HTTP 建立阶段的离线测试。
//!
//! 覆盖：出站请求断言（url/method/header/body）、建立前失败（编码、
//! 连接、超时、取消）、非 2xx 状态码分类与错误正文采样、敏感信息脱敏。
//! 全部经 `RecordedTransport` 回放，无真实网络。

use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_model::{
    GenerationConfig, ModelCallContext, ModelError, ModelRequest, ModelService,
    ModelTransportErrorKind, ProviderOptions, ReasoningConfig, ReasoningEffort,
    SystemPromptSnapshot, TraceContext,
};
use agent_testkit::{
    BodyStep, CancelGate, EventCollector, RecordedResponse, RecordedTransport,
    RecordedTransportError,
};
use agent_types::{
    ConversationMessage, ConversationSnapshot, MessageId, PartId, ProviderId, TextPart, ToolChoice,
    UserMessage, UserPart,
};
use futures_util::StreamExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{
    BearerCredential, ChatRequest, ChatStreamOptions, ObservedTransport, OpenAiCompatibleService,
    Profile, ProviderWireEvent, ProviderWireObserver, RecordedWireRequest, ReqwestTransport,
    Transport, TransportError, TransportFuture, TransportRequest, TransportResponse,
    TransportTimeouts, encode_request,
};

const BASE_URL: &str = "https://api.openai.test";
const TOKEN: &str = "sk-transport-test-marker";
const CONTEXT_OVERFLOW_BODY: &str = include_str!("../fixtures/errors/context_overflow.json");

/// 用户正文中的标记文本；任何错误与事件都不得携带它（防请求正文泄漏）。
const USER_MARKER: &str = "USER-SECRET-MARKER";

const OK_SSE_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-t\",\"model\":\"gpt-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-t\",\"model\":\"gpt-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

fn provider_id(value: &str) -> ProviderId {
    ProviderId::new(value).expect("valid provider id")
}

fn base_profile() -> Profile {
    Profile::openai_compatible(provider_id("openai"))
}

fn service_with(transport: &Arc<RecordedTransport>) -> OpenAiCompatibleService {
    OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_profile(),
        transport.clone(),
    )
    .expect("test base URL should be valid")
}

fn simple_request() -> ModelRequest {
    ModelRequest {
        system: SystemPromptSnapshot::default(),
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            id: MessageId::new("message_1").expect("valid message id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new("text_1").expect("valid part id"),
                text: format!("say hi ({USER_MARKER})"),
            })],
        })]),
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: ProviderOptions::new(),
    }
}

/// 直接返回固定错误的桩 Transport，用于注入 RecordedTransport 表达不了的失败。
struct StubTransport {
    error: TransportError,
}

impl Transport for StubTransport {
    fn execute<'a>(&'a self, _request: TransportRequest) -> TransportFuture<'a> {
        let error = self.error.clone();
        Box::pin(async move { Err(error) })
    }
}

/// 捕获控制面 Trace，同时返回固定成功响应；用于证明 Trace 不需要进入 HTTP 字段。
struct TraceCaptureTransport {
    trace: Mutex<Option<TraceContext>>,
}

impl Transport for TraceCaptureTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        *self
            .trace
            .lock()
            .expect("trace lock should not be poisoned") = request.trace;
        Box::pin(async move {
            Ok(TransportResponse {
                status: 200,
                headers: Vec::new(),
                body: Box::pin(futures_util::stream::once(async {
                    Ok(OK_SSE_BODY.as_bytes().to_vec())
                })),
            })
        })
    }
}

#[derive(Default)]
/// 测试观察器只把事件快速复制进内存，不参与 Transport 结果。
struct WireCollector {
    events: Mutex<Vec<ProviderWireEvent>>,
}

impl WireCollector {
    fn events(&self) -> Vec<ProviderWireEvent> {
        self.events
            .lock()
            .expect("wire event lock should not be poisoned")
            .clone()
    }
}

impl ProviderWireObserver for WireCollector {
    fn observe(&self, event: ProviderWireEvent) {
        self.events
            .lock()
            .expect("wire event lock should not be poisoned")
            .push(event);
    }
}

/// 模拟宿主队列已满时直接丢弃观察事件。
struct DroppingObserver;

impl ProviderWireObserver for DroppingObserver {
    fn observe(&self, _event: ProviderWireEvent) {}
}

/// 永不自行完成的 Transport，用于精确验证等待响应头时取消不会派生后台工作。
struct HangingTransport {
    entered: Arc<Notify>,
}

impl Transport for HangingTransport {
    fn execute<'a>(&'a self, _request: TransportRequest) -> TransportFuture<'a> {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.notify_one();
            std::future::pending().await
        })
    }
}

fn wire_request(trace: Option<TraceContext>) -> TransportRequest {
    TransportRequest {
        trace,
        method: "POST".to_owned(),
        url: format!("{BASE_URL}/chat/completions"),
        headers: vec![
            ("Authorization".to_owned(), format!("Bearer {TOKEN}")),
            ("content-type".to_owned(), "application/json".to_owned()),
        ],
        body: br#"{"model":"gpt-test"}"#.to_vec(),
    }
}

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
        &encode_request(&request, &base_profile(), "gpt-test").expect("encode succeeds"),
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
    let service = OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_profile(),
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
        let error = OpenAiCompatibleService::with_transport(
            base_url,
            BearerCredential::new(TOKEN),
            "gpt-test",
            128_000,
            base_profile(),
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
        OpenAiCompatibleService::with_transport(
            BASE_URL,
            BearerCredential::new(TOKEN),
            "gpt-test",
            128_000,
            base_profile(),
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
        &encode_request(&request, &base_profile(), "gpt-test").expect("encode should succeed"),
    )
    .expect("encoded request should serialize");
    let second = serde_json::to_vec(
        &encode_request(&request, &base_profile(), "gpt-test").expect("encode should succeed"),
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
    let observed_service = OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_profile(),
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
    let service = OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_profile(),
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
