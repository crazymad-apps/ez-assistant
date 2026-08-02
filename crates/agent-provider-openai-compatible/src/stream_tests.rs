//! SSE 解析与流式 Adapter 的离线测试。
//!
//! 覆盖：SSE frame 语义（多行 data 合并、跨字节 chunk、单 chunk 多 frame、
//! 空行、心跳、`[DONE]`）、端到端离线回放、畸形 JSON、流中断、无终态关闭、
//! 流中取消、能力推导与敏感信息脱敏。全部经 `RecordedTransport` 回放，
//! 无真实网络，无 sleep 竞态。

use std::sync::Arc;

use agent_model::{
    GenerationConfig, ModelCallContext, ModelError, ModelEvent, ModelRequest, ModelService,
    ProviderOptions,
};
use agent_testkit::{BodyStep, CancelGate, EventCollector, RecordedResponse, RecordedTransport};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProtocolId, ProviderId, ReasoningPart, TextPart, TokenUsage,
    ToolCall, ToolCallId, ToolChoice, ToolName, UserMessage, UserPart,
};
use serde_json::json;

use crate::{BearerCredential, OpenAiCompatibleService, Profile, SseFrame, SseParser};

const BASE_URL: &str = "https://api.deepseek.test";
const TOKEN: &str = "sk-stream-test-marker";

/// 用户正文中的标记文本；任何事件与错误都不得携带它（防请求正文泄漏）。
const USER_MARKER: &str = "USER-SECRET-MARKER";

/// reasoning + text + tool call + usage + `[DONE]` 的完整回放。
const REPLAY_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Need the date.\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Let me check.\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_date\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-reasoner\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n",
    "data: [DONE]\n\n",
);

/// DeepSeek thinking Profile 下不合法的工具调用响应：缺少 `reasoning_content`。
const TOOL_CALL_WITHOUT_REASONING_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-missing-reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"shell\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-missing-reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// 纯文本 Turn 的各个 frame，用于组合不同的字节投递形状。
const FRAME_HELLO: &str = "data: {\"id\":\"chatcmpl-2\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello, \"},\"finish_reason\":null}]}\n\n";
const FRAME_WORLD: &str = "data: {\"id\":\"chatcmpl-2\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"world!\"},\"finish_reason\":null}]}\n\n";
const FRAME_FINISH: &str = "data: {\"id\":\"chatcmpl-2\",\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
const FRAME_DONE: &str = "data: [DONE]\n\n";

fn simple_text_body() -> String {
    format!("{FRAME_HELLO}{FRAME_WORLD}{FRAME_FINISH}{FRAME_DONE}")
}

const CONTEXT_OVERFLOW_FRAME: &str = include_str!("../fixtures/errors/context_overflow_stream.sse");

fn provider_id(value: &str) -> ProviderId {
    ProviderId::new(value).expect("valid provider id")
}

fn protocol_id(value: &str) -> ProtocolId {
    ProtocolId::new(value).expect("valid protocol id")
}

fn message_id(value: &str) -> MessageId {
    MessageId::new(value).expect("valid message id")
}

fn part_id(value: &str) -> PartId {
    PartId::new(value).expect("valid part id")
}

fn call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid call id")
}

fn tool_name(value: &str) -> ToolName {
    ToolName::new(value).expect("valid tool name")
}

/// 带 reasoning 字段的方言（DeepSeek 形态字面量；具名构造见 [`Profile::deepseek`]）。
fn reasoning_profile() -> Profile {
    Profile {
        provider: provider_id("deepseek"),
        protocol: protocol_id("openai.chat_completions"),
        reasoning_content_field: Some("reasoning_content".to_owned()),
        reasoning_effort_field: Some("reasoning_effort".to_owned()),
        supports_temperature: true,
        supports_top_p: true,
        supports_stop: true,
        max_output_tokens_field: Some("max_tokens".to_owned()),
        supports_tool_choice: true,
        tool_calls_require_reasoning: false,
        cached_input_tokens_field: None,
    }
}

fn service_with(transport: &Arc<RecordedTransport>, profile: Profile) -> OpenAiCompatibleService {
    OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        "deepseek-reasoner",
        128_000,
        profile,
        transport.clone(),
    )
}

fn replay_service(body: impl Into<Vec<u8>>) -> (OpenAiCompatibleService, Arc<RecordedTransport>) {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        body.into(),
    ))]));
    (service_with(&transport, reasoning_profile()), transport)
}

fn request() -> ModelRequest {
    ModelRequest {
        system: vec![],
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            id: message_id("message_1"),
            parts: vec![UserPart::Text(TextPart {
                id: part_id("text_1"),
                text: format!("What date is it? ({USER_MARKER})"),
            })],
        })]),
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: ProviderOptions::new(),
    }
}

fn frame(data: &str) -> SseFrame {
    SseFrame {
        data: data.to_owned(),
    }
}

#[test]
fn parser_merges_multiline_data_and_dispatches_on_blank_line() {
    let mut parser = SseParser::new();
    let frames = parser
        .push(b"data: line1\ndata: line2\n\ndata: next\n\n")
        .expect("parse succeeds");
    assert_eq!(frames, vec![frame("line1\nline2"), frame("next")]);
}

#[test]
fn parser_buffers_partial_lines_and_split_utf8_across_chunks() {
    let mut parser = SseParser::new();
    // 单条 data 跨字节 chunk；é 的两个字节被切开。
    let bytes = "data: héllo\n\n".as_bytes();
    let split = "data: h".len() + 1;
    assert!(
        parser
            .push(&bytes[..split])
            .expect("first chunk")
            .is_empty()
    );
    let frames = parser.push(&bytes[split..]).expect("second chunk");
    assert_eq!(frames, vec![frame("héllo")]);
}

#[test]
fn parser_emits_multiple_frames_from_one_chunk() {
    let mut parser = SseParser::new();
    let frames = parser
        .push(b"data: a\n\ndata: b\n\ndata: c\n\n")
        .expect("parse succeeds");
    assert_eq!(frames, vec![frame("a"), frame("b"), frame("c")]);
}

#[test]
fn parser_ignores_heartbeats_comments_and_empty_events() {
    let mut parser = SseParser::new();
    // 心跳/注释行、没有 data 的事件、多余的空行都不产出 frame。
    let frames = parser
        .push(b": heartbeat\n\n\n\ndata: x\n\n: another\n\n")
        .expect("parse succeeds");
    assert_eq!(frames, vec![frame("x")]);
}

#[test]
fn parser_ignores_event_id_and_retry_fields() {
    let mut parser = SseParser::new();
    let frames = parser
        .push(b"event: message\nid: 42\nretry: 100\ndata: x\n\n")
        .expect("parse succeeds");
    assert_eq!(frames, vec![frame("x")]);
}

#[test]
fn parser_passes_done_payload_through_without_special_casing() {
    let mut parser = SseParser::new();
    let frames = parser.push(b"data: [DONE]\n\n").expect("parse succeeds");
    assert_eq!(frames, vec![frame("[DONE]")]);
}

#[test]
fn parser_handles_crlf_line_endings() {
    let mut parser = SseParser::new();
    let frames = parser
        .push(b"data: a\r\ndata: b\r\n\r\n")
        .expect("parse succeeds");
    assert_eq!(frames, vec![frame("a\nb")]);
}

#[test]
fn parser_rejects_invalid_utf8_as_protocol_error() {
    let mut parser = SseParser::new();
    let error = parser.push(b"data: \xff\xfe\n\n").expect_err("must fail");
    assert!(matches!(error, ModelError::Protocol(_)));
}

#[tokio::test]
async fn stream_replays_reasoning_text_and_tool_call_turn_end_to_end() {
    let (service, _transport) = replay_service(REPLAY_BODY);
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;

    let expected_message = AssistantMessage {
        id: message_id("chatcmpl-1"),
        model: ModelIdentity::new(provider_id("deepseek"), "deepseek-reasoner"),
        parts: vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: part_id("part_1"),
                text: "Need the date.".to_owned(),
            }),
            AssistantPart::Text(TextPart {
                id: part_id("part_2"),
                text: "Let me check.".to_owned(),
            }),
            AssistantPart::ToolCall(ToolCall {
                id: call_id("call_1"),
                name: tool_name("get_date"),
                arguments: json!({"city": "Paris"}),
            }),
        ],
        finish_reason: FinishReason::ToolCalls,
        usage: Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cached_input_tokens: None,
            reasoning_tokens: Some(3),
        }),
    };
    let expected = vec![
        ModelEvent::TurnStarted {
            message_id: message_id("chatcmpl-1"),
            model: ModelIdentity::new(provider_id("deepseek"), "deepseek-reasoner"),
        },
        ModelEvent::ReasoningStarted {
            id: part_id("part_1"),
        },
        ModelEvent::ReasoningDelta {
            id: part_id("part_1"),
            delta: "Need the date.".to_owned(),
        },
        ModelEvent::ReasoningFinished {
            id: part_id("part_1"),
        },
        ModelEvent::TextStarted {
            id: part_id("part_2"),
        },
        ModelEvent::TextDelta {
            id: part_id("part_2"),
            delta: "Let me check.".to_owned(),
        },
        // text part 保持开放，tool call 事件与之交错；finish_reason 到达时统一收尾。
        ModelEvent::ToolCallStarted {
            id: call_id("call_1"),
            name: tool_name("get_date"),
        },
        ModelEvent::ToolCallDelta {
            id: call_id("call_1"),
            arguments_delta: "{\"city\":\"Paris\"}".to_owned(),
        },
        ModelEvent::TextFinished {
            id: part_id("part_2"),
        },
        ModelEvent::ToolCallFinished {
            id: call_id("call_1"),
            arguments: json!({"city": "Paris"}),
        },
        ModelEvent::UsageUpdated {
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                cached_input_tokens: None,
                reasoning_tokens: Some(3),
            },
        },
        ModelEvent::TurnFinished {
            message: expected_message,
        },
    ];
    assert_eq!(collected.events(), expected.as_slice());
    collected.assert_single_terminal();
}

#[tokio::test]
async fn deepseek_stream_rejects_tool_call_without_reasoning_as_protocol_terminal() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        TOOL_CALL_WITHOUT_REASONING_BODY,
    ))]));
    let service = service_with(&transport, Profile::deepseek());
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;

    let ModelError::Protocol(message) = collected.assert_failed() else {
        panic!("expected protocol failure");
    };
    assert!(message.contains("no reasoning content"));
    assert!(
        !collected
            .events()
            .iter()
            .any(|event| matches!(event, ModelEvent::TurnFinished { .. }))
    );
    collected.assert_single_terminal();
}

#[tokio::test]
async fn stream_assembles_frames_split_across_body_chunks() {
    // 任意 7 字节边界切分（可能切在 frame 中间与 UTF-8 序列中间）。
    let steps: Vec<BodyStep> = simple_text_body()
        .as_bytes()
        .chunks(7)
        .map(|chunk| BodyStep::Chunk(chunk.to_vec()))
        .collect();
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200, steps,
    ))]));
    let service = service_with(&transport, reasoning_profile());
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    let message = collected.assert_finished();
    assert_eq!(
        message.parts,
        vec![AssistantPart::Text(TextPart {
            id: part_id("part_1"),
            text: "Hello, world!".to_owned(),
        })]
    );
    assert_eq!(message.finish_reason, FinishReason::Stop);
    collected.assert_single_terminal();
}

#[tokio::test]
async fn stream_ignores_trailing_data_after_done() {
    // [DONE] 之后的 frame（即使不是合法 JSON）一律忽略。
    let body = format!("{}data: {{broken after done\n\n", simple_text_body());
    let (service, _transport) = replay_service(body);
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    collected.assert_finished();
    collected.assert_single_terminal();
}

#[tokio::test]
async fn malformed_chunk_json_fails_with_single_protocol_terminal() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![
            BodyStep::Chunk(FRAME_HELLO.as_bytes().to_vec()),
            BodyStep::Chunk(b"data: {not valid json\n\n".to_vec()),
        ],
    ))]));
    let service = service_with(&transport, reasoning_profile());
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    let ModelError::Protocol(message) = collected.assert_failed() else {
        panic!("expected protocol failure");
    };
    assert!(message.contains("not a valid chat chunk"));
    // 已产出的正常事件保留，失败是唯一终态且之后没有事件。
    assert!(matches!(
        collected.events().last(),
        Some(ModelEvent::TurnFailed { .. })
    ));
    collected.assert_single_terminal();
}

#[tokio::test]
async fn interrupted_body_fails_with_transport_terminal() {
    // 响应中途断流：字节流的 Err 项是传输错误，立即以 TurnFailed(Transport)
    // 受控结束，而不是交给 finalize 按缺 finish_reason 报 Protocol。
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![
            BodyStep::Chunk(FRAME_HELLO.as_bytes().to_vec()),
            BodyStep::Fail("connection reset by peer".to_owned()),
            BodyStep::Chunk(FRAME_WORLD.as_bytes().to_vec()),
        ],
    ))]));
    let service = service_with(&transport, reasoning_profile());
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    let ModelError::Transport(message) = collected.assert_failed() else {
        panic!("expected transport failure");
    };
    assert!(message.contains("connection reset by peer"));
    collected.assert_single_terminal();
}

#[tokio::test]
async fn interrupted_after_finish_chunk_fails_with_transport() {
    // finish chunk 之后、[DONE] 之前断流：即使已收到 finish_reason 也不得把
    // 传输中断误报为成功，必须以 TurnFailed(Transport) 受控结束。
    let body = format!("{FRAME_HELLO}{FRAME_WORLD}{FRAME_FINISH}");
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![
            BodyStep::Chunk(body.into_bytes()),
            BodyStep::Fail("connection reset by peer".to_owned()),
        ],
    ))]));
    let service = service_with(&transport, reasoning_profile());
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    let ModelError::Transport(message) = collected.assert_failed() else {
        panic!("expected transport failure, not a successful finish");
    };
    assert!(message.contains("connection reset by peer"));
    collected.assert_single_terminal();
}

#[tokio::test]
async fn closed_without_finish_reason_fails_with_protocol() {
    // 字节流正常结束但没有终态 chunk（无 [DONE]、无 finish_reason）。
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        format!("{FRAME_HELLO}{FRAME_WORLD}"),
    ))]));
    let service = service_with(&transport, reasoning_profile());
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    let ModelError::Protocol(message) = collected.assert_failed() else {
        panic!("expected protocol failure");
    };
    assert!(message.contains("finish_reason"));
    collected.assert_single_terminal();
}

#[tokio::test]
async fn empty_body_fails_with_protocol() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![],
    ))]));
    let service = service_with(&transport, reasoning_profile());
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    // 唯一的事件就是受控失败终态。
    assert_eq!(collected.events().len(), 1);
    let ModelError::Protocol(message) = collected.assert_failed() else {
        panic!("expected protocol failure");
    };
    assert!(message.contains("before any chunk"));
}

#[tokio::test]
async fn cancellation_during_stream_yields_single_cancelled_terminal() {
    let gate = CancelGate::new();
    // TurnStarted + ReasoningStarted 两个事件之后取消；回放是单个字节 chunk，
    // 证明取消检查逐事件生效而不是只在 chunk 边界生效。
    gate.cancel_after(2);
    let (service, _transport) = replay_service(REPLAY_BODY);
    let context = ModelCallContext::new(gate.token());
    let stream = service
        .stream(request(), context)
        .await
        .expect("stream established");
    let collected = EventCollector::collect(gate.watch(stream)).await;

    assert!(gate.fired());
    assert_eq!(collected.events().len(), 3);
    assert!(matches!(
        collected.events()[0],
        ModelEvent::TurnStarted { .. }
    ));
    assert!(matches!(
        collected.events()[1],
        ModelEvent::ReasoningStarted { .. }
    ));
    assert_eq!(
        collected.events()[2],
        ModelEvent::TurnFailed {
            error: ModelError::Cancelled
        }
    );
    // 取消后不再有事件（收集到 None 才结束），实现不派生任何后台任务。
    assert_eq!(gate.emitted(), 3);
    collected.assert_single_terminal();
}

#[tokio::test]
async fn structured_context_overflow_frame_yields_failed_terminal() {
    let (service, _transport) = replay_service(CONTEXT_OVERFLOW_FRAME);
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("http stream established");
    let collected = EventCollector::collect_validated(stream).await;

    assert!(
        collected.events().iter().any(
            |event| matches!(event, ModelEvent::TextDelta { delta, .. } if delta == "partial")
        )
    );
    assert_eq!(
        collected.assert_failed(),
        &ModelError::ContextOverflow {
            message: "stream context limit".to_owned(),
        }
    );
    collected.assert_single_terminal();
}

#[tokio::test]
async fn capabilities_follow_the_profile() {
    let transport = Arc::new(RecordedTransport::new([]));
    let with_reasoning = service_with(&transport, reasoning_profile());
    assert!(with_reasoning.capabilities().reasoning);
    assert!(with_reasoning.capabilities().tool_calls);
    assert!(with_reasoning.capabilities().streaming);

    let base = service_with(
        &transport,
        Profile::openai_compatible(provider_id("openai")),
    );
    assert!(!base.capabilities().reasoning);
    assert!(base.capabilities().tool_calls);
    assert!(base.capabilities().streaming);
}

#[tokio::test]
async fn sensitive_data_never_enters_debug_errors_or_events() {
    // credential 的 Debug 脱敏。
    let credential = BearerCredential::new(TOKEN);
    let debug = format!("{credential:?}");
    assert!(!debug.contains(TOKEN));

    // 正常回放：事件序列不含 token 与请求正文标记。
    let (service, _transport) = replay_service(REPLAY_BODY);
    let stream = service
        .stream(request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;
    collected.assert_finished();
    let dump = format!("{:?}", collected.events());
    assert!(!dump.contains(TOKEN));
    assert!(!dump.contains(USER_MARKER));

    // 建立前失败（401）：错误文本不含 token 与请求正文标记。
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        401,
        r#"{"error":{"message":"bad credential","type":"authentication_error"}}"#,
    ))]));
    let service = service_with(&transport, reasoning_profile());
    let error = service
        .stream(request(), ModelCallContext::default())
        .await
        .err()
        .expect("401 must fail");
    let text = error.to_string();
    assert!(!text.contains(TOKEN));
    assert!(!text.contains(USER_MARKER));
}
