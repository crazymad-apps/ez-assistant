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
    ProtocolAdapter, ProviderWireEvent, ProviderWireObserver, RecordedWireRequest,
    ReqwestTransport, Transport, TransportError, TransportFuture, TransportRequest,
    TransportResponse, TransportTimeouts, encode_request,
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

fn base_adapter() -> ProtocolAdapter {
    ProtocolAdapter::openai_compatible(provider_id("openai"))
}

fn service_with(transport: &Arc<RecordedTransport>) -> OpenAiCompatibleService {
    OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "gpt-test",
        128_000,
        base_adapter(),
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

mod errors;
mod observation;
