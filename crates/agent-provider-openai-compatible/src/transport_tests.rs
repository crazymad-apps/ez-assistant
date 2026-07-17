//! Transport 边界与 HTTP 建立阶段的离线测试。
//!
//! 覆盖：出站请求断言（url/method/header/body）、建立前失败（编码、
//! 连接、超时、取消）、非 2xx 状态码分类与错误正文采样、敏感信息脱敏。
//! 全部经 `RecordedTransport` 回放，无真实网络。

use std::{sync::Arc, time::Duration};

use agent_model::{
    GenerationConfig, ModelCallContext, ModelError, ModelRequest, ModelService, ProviderOptions,
    ReasoningConfig, ReasoningEffort,
};
use agent_testkit::{
    CancelGate, EventCollector, RecordedResponse, RecordedTransport, RecordedTransportError,
};
use agent_types::{
    ConversationMessage, ConversationSnapshot, MessageId, PartId, ProviderId, TextPart, ToolChoice,
    UserMessage, UserPart,
};

use crate::{
    BearerCredential, ChatRequest, ChatStreamOptions, OpenAiCompatibleService, Profile,
    ReqwestTransport, Transport, TransportError, TransportFuture, TransportRequest,
    TransportTimeouts, encode_request,
};

const BASE_URL: &str = "https://api.openai.test";
const TOKEN: &str = "sk-transport-test-marker";

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
        base_profile(),
        transport.clone(),
    )
}

fn simple_request() -> ModelRequest {
    ModelRequest {
        system: vec![],
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
    let ModelError::Transport(message) = error else {
        panic!("expected transport error, got {error:?}");
    };
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
        base_profile(),
        Arc::new(StubTransport {
            error: TransportError::Timeout,
        }),
    );
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("timeout must fail");
    assert_eq!(error, ModelError::Transport("request timed out".to_owned()));
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
    ))]));
    let service = service_with(&transport);
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("429 must fail");
    assert_eq!(error, ModelError::RateLimited("slow down".to_owned()));
}

#[tokio::test]
async fn http_500_maps_to_provider_with_status() {
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
    let ModelError::Provider { message, status } = error else {
        panic!("expected provider error, got {error:?}");
    };
    assert_eq!(status, Some(500));
    // 非结构化正文不进入错误文本。
    assert!(!message.contains("BODY-MARKER"));
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

    // 500 + rate_limit 类型：按正文细化为 RateLimited。
    let error = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .err()
        .expect("500 must fail");
    assert_eq!(error, ModelError::RateLimited("quota exhausted".to_owned()));

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
        ModelError::Provider {
            message: "provider returned status 500 without a structured error body".to_owned(),
            status: Some(500),
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
        base_profile(),
        defaults,
    )
    .expect("service builds on reqwest transport");
    assert!(!service.capabilities().reasoning);
    assert!(service.capabilities().tool_calls);
    assert!(service.capabilities().streaming);
}

#[test]
fn transport_request_debug_redacts_authorization_and_body() {
    let request = TransportRequest {
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
