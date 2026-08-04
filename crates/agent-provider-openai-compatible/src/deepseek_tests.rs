//! DeepSeek thinking/tool-call 契约测试（M6）。
//!
//! 依据官方文档（2026-07 核对）：
//!
//! - <https://api-docs.deepseek.com/guides/thinking_mode/>：thinking 开关为请求根部的
//!   `thinking: {"type": "enabled"}` 对象（默认 `enabled`，适用 `deepseek-v4-flash` /
//!   `deepseek-v4-pro`）；thinking 模式下 `temperature` / `top_p` 不生效；带 tool calls
//!   的 assistant 消息必须在后续请求中完整回传 `reasoning_content`，缺失时 API 返回 400。
//! - <https://api-docs.deepseek.com/api/create-chat-completion/>：`reasoning_content`
//!   消息字段；usage 的扁平 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`。
//!
//! 离线契约测试用 `RecordedTransport` 回放 `fixtures/deepseek/` 的可审阅 SSE
//! transcript，并断言捕获的第二轮请求与预期 JSON 完全一致，全程无真实网络。
//! 真实 API smoke test 被 `#[ignore]`，运行方式见 crate 文档（lib.rs）。

use std::sync::Arc;

use agent_model::{
    GenerationConfig, ModelCallContext, ModelError, ModelEvent, ModelRequest, ModelService,
    ProviderOptions, ReasoningConfig, ReasoningEffort, SystemPromptSnapshot,
};
use agent_testkit::{
    EventCollector, RecordedRequest, RecordedResponse, RecordedTransport, validate_request,
    validate_response,
};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, PartId, ProtocolId, ProviderId, ReasoningPart, TextPart, TokenUsage,
    ToolCall, ToolCallId, ToolChoice, ToolDefinition, ToolMessage, ToolName, ToolResult,
    ToolResultContent, ToolResultStatus, UserMessage, UserPart,
};
use serde_json::{Value, json};

use crate::{
    BearerCredential, ChatResponse, OpenAiCompatibleService, Profile, TransportTimeouts,
    decode_response, encode_request,
};

const BASE_URL: &str = "https://api.deepseek.com";
/// 测试凭据标记；只进入被捕获请求的 `Authorization` header，不写入任何 fixture 文件。
const TOKEN: &str = "deepseek-contract-test-token";

/// 第一轮 transcript：reasoning + content + 分片 tool call + 扁平 cache usage。
const TURN_1_TRANSCRIPT: &str = include_str!("../fixtures/deepseek/turn_1_tool_call.sse");
/// 第二轮 transcript：reasoning + 最终回答 + 扁平 cache usage。
const TURN_2_TRANSCRIPT: &str = include_str!("../fixtures/deepseek/turn_2_final_answer.sse");
/// 预期的第二轮请求 JSON。
const TURN_2_REQUEST_JSON: &str = include_str!("../fixtures/deepseek/turn_2_request.json");

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

/// 离线契约测试的目标模型名；线上模型名由服务构造期绑定，编码时显式传入。
const MODEL: &str = "deepseek-v4-pro";

fn weather_tool() -> ToolDefinition {
    ToolDefinition {
        name: tool_name("get_weather"),
        description: "Get the current weather of a city.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {"city": {"type": "string", "description": "The city name."}},
            "required": ["city"],
        }),
    }
}

fn user_message(id: &str, text: &str) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        id: message_id(id),
        parts: vec![UserPart::Text(TextPart {
            id: part_id(&format!("{id}_text")),
            text: text.to_owned(),
        })],
    })
}

fn tool_result_message(id: &str, call: &str, content: &str) -> ConversationMessage {
    ConversationMessage::Tool(ToolMessage {
        id: message_id(id),
        result: ToolResult {
            call_id: call_id(call),
            status: ToolResultStatus::Success,
            content: ToolResultContent::Text(content.to_owned()),
        },
    })
}

fn assistant_message(parts: Vec<AssistantPart>) -> AssistantMessage {
    AssistantMessage {
        id: message_id("turn_1"),
        model: ModelIdentity::new(provider_id("deepseek"), "deepseek-v4-pro"),
        parts,
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

/// thinking 开关经 `deepseek` 命名空间下发；编码器按命名空间合并进请求根（M4 机制）。
fn thinking_options() -> ProviderOptions {
    let mut options = ProviderOptions::new();
    options
        .insert("deepseek", json!({"thinking": {"type": "enabled"}}))
        .expect("valid provider options");
    options
}

fn turn_1_request() -> ModelRequest {
    ModelRequest {
        system: SystemPromptSnapshot::default(),
        conversation: ConversationSnapshot::new(vec![user_message(
            "message_1",
            "How is the weather in Paris today?",
        )]),
        tools: vec![weather_tool()],
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: thinking_options(),
    }
}

fn service_with(transport: &Arc<RecordedTransport>) -> OpenAiCompatibleService {
    OpenAiCompatibleService::with_transport(
        BASE_URL,
        BearerCredential::new(TOKEN),
        MODEL,
        128_000,
        Profile::deepseek(),
        transport.clone(),
    )
    .expect("test base URL should be valid")
}

#[test]
fn fixtures_are_reviewable_and_credential_free() {
    // fixture 不得携带敏感 header、疑似 credential 或不可再现动态值（M3 校验职责）。
    let turn_1 = RecordedResponse::new(200, TURN_1_TRANSCRIPT);
    validate_response(&turn_1).expect("turn 1 transcript must be clean");
    let turn_2 = RecordedResponse::new(200, TURN_2_TRANSCRIPT);
    validate_response(&turn_2).expect("turn 2 transcript must be clean");
    // 预期请求 fixture 只有请求体；敏感 header 由 Adapter 注入，不进 fixture。
    let request = RecordedRequest::new("POST", "https://api.deepseek.com/chat/completions")
        .with_header("content-type", "application/json")
        .with_body(TURN_2_REQUEST_JSON);
    validate_request(&request).expect("expected request fixture must be clean");
}

#[tokio::test]
async fn turn_1_replays_reasoning_text_tool_call_and_cache_usage() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        TURN_1_TRANSCRIPT,
    ))]));
    let service = service_with(&transport);
    let stream = service
        .stream(turn_1_request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;

    let expected_usage = TokenUsage {
        input_tokens: 128,
        output_tokens: 42,
        total_tokens: 170,
        // DeepSeek 扁平的 prompt_cache_hit_tokens 映射进 cached_input_tokens。
        cached_input_tokens: Some(64),
        reasoning_tokens: Some(20),
    };
    let expected_message = AssistantMessage {
        id: message_id("chatcmpl-contract-1"),
        model: ModelIdentity::new(provider_id("deepseek"), "deepseek-v4-pro"),
        // reasoning、content、tool call 全保留且顺序正确。
        parts: vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: part_id("part_1"),
                text: "The user asks for the weather in Paris. I should call get_weather first."
                    .to_owned(),
            }),
            AssistantPart::Text(TextPart {
                id: part_id("part_2"),
                text: "Let me check the weather.".to_owned(),
            }),
            AssistantPart::ToolCall(ToolCall {
                id: call_id("call_get_weather_1"),
                name: tool_name("get_weather"),
                arguments: json!({"city": "Paris"}),
            }),
        ],
        finish_reason: FinishReason::ToolCalls,
        usage: Some(expected_usage.clone()),
    };
    let expected = vec![
        ModelEvent::TurnStarted {
            message_id: message_id("chatcmpl-contract-1"),
            model: ModelIdentity::new(provider_id("deepseek"), "deepseek-v4-pro"),
        },
        ModelEvent::ReasoningStarted {
            id: part_id("part_1"),
        },
        ModelEvent::ReasoningDelta {
            id: part_id("part_1"),
            delta: "The user asks for the weather in Paris.".to_owned(),
        },
        ModelEvent::ReasoningDelta {
            id: part_id("part_1"),
            delta: " I should call get_weather first.".to_owned(),
        },
        ModelEvent::ReasoningFinished {
            id: part_id("part_1"),
        },
        ModelEvent::TextStarted {
            id: part_id("part_2"),
        },
        ModelEvent::TextDelta {
            id: part_id("part_2"),
            delta: "Let me check ".to_owned(),
        },
        ModelEvent::TextDelta {
            id: part_id("part_2"),
            delta: "the weather.".to_owned(),
        },
        // text part 保持开放，tool call 事件与之交错；finish_reason 到达时统一收尾。
        ModelEvent::ToolCallStarted {
            id: call_id("call_get_weather_1"),
            name: tool_name("get_weather"),
        },
        ModelEvent::ToolCallDelta {
            id: call_id("call_get_weather_1"),
            arguments_delta: "{\"city\":\"Par".to_owned(),
        },
        ModelEvent::ToolCallDelta {
            id: call_id("call_get_weather_1"),
            arguments_delta: "is\"}".to_owned(),
        },
        ModelEvent::TextFinished {
            id: part_id("part_2"),
        },
        ModelEvent::ToolCallFinished {
            id: call_id("call_get_weather_1"),
            arguments: json!({"city": "Paris"}),
        },
        ModelEvent::UsageUpdated {
            usage: expected_usage,
        },
        ModelEvent::TurnFinished {
            message: expected_message,
        },
    ];
    assert_eq!(collected.events(), expected.as_slice());
    collected.assert_single_terminal();

    // thinking 开关按命名空间合并进请求根（M4 机制对 DeepSeek profile 生效）。
    let requests = transport.take_requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("request body is json");
    assert_eq!(body["thinking"], json!({"type": "enabled"}));
}

#[tokio::test]
async fn turn_2_request_round_trips_reasoning_tool_calls_and_thinking() {
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(200, TURN_1_TRANSCRIPT)),
        Ok(RecordedResponse::new(200, TURN_2_TRANSCRIPT)),
    ]));
    let service = service_with(&transport);

    // 第一轮回放，得到完整的 AssistantMessage（reasoning + content + tool call）。
    let stream = service
        .stream(turn_1_request(), ModelCallContext::default())
        .await
        .expect("turn 1 stream established");
    let turn_1 = EventCollector::collect_validated(stream).await;
    let assistant = turn_1.assert_finished().clone();

    // 第二轮：user + 第一轮 AssistantMessage + ToolResult，thinking 开关随请求下发。
    let turn_2_request = ModelRequest {
        system: SystemPromptSnapshot::default(),
        conversation: ConversationSnapshot::new(vec![
            user_message("message_1", "How is the weather in Paris today?"),
            ConversationMessage::Assistant(assistant),
            tool_result_message(
                "message_2",
                "call_get_weather_1",
                "Cloudy, 7 to 13 degrees Celsius.",
            ),
        ]),
        tools: vec![weather_tool()],
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: thinking_options(),
    };
    let stream = service
        .stream(turn_2_request, ModelCallContext::default())
        .await
        .expect("turn 2 stream established");
    let turn_2 = EventCollector::collect_validated(stream).await;

    // 第二轮 transcript 回放完成：reasoning + 最终回答，usage 命中缓存。
    let message = turn_2.assert_finished();
    assert_eq!(
        message.parts,
        vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: part_id("part_1"),
                text: "The weather result is in. I can answer now.".to_owned(),
            }),
            AssistantPart::Text(TextPart {
                id: part_id("part_2"),
                text: "It is cloudy in Paris, 7 to 13 degrees Celsius.".to_owned(),
            }),
        ]
    );
    assert_eq!(message.finish_reason, FinishReason::Stop);
    assert_eq!(
        message.usage,
        Some(TokenUsage {
            input_tokens: 176,
            output_tokens: 30,
            total_tokens: 206,
            cached_input_tokens: Some(128),
            reasoning_tokens: Some(8),
        })
    );
    turn_2.assert_single_terminal();

    // 捕获的第二轮请求与预期 fixture 完全一致：`reasoning_content`、`content`、
    // `tool_calls`（含 id 与顺序）、tool 消息的 `tool_call_id` 配对、
    // thinking 对象合并在请求根。
    let requests = transport.take_requests();
    assert_eq!(requests.len(), 2);
    let captured: Value =
        serde_json::from_slice(&requests[1].body).expect("turn 2 request body is json");
    let expected: Value =
        serde_json::from_str(TURN_2_REQUEST_JSON).expect("expected request fixture is json");
    assert_eq!(captured, expected);
}

#[tokio::test]
async fn encode_rejects_tool_call_assistant_without_reasoning() {
    // 官方文档：thinking 模式带 tool calls 的 assistant 消息必须在后续请求中完整回传
    // `reasoning_content`，否则 API 返回 400；编码侧提前以 Config 显式失败。
    let transport = Arc::new(RecordedTransport::new([]));
    let service = service_with(&transport);
    let mut request = turn_1_request();
    request.conversation = ConversationSnapshot::new(vec![
        user_message("message_1", "How is the weather in Paris today?"),
        ConversationMessage::Assistant(assistant_message(vec![AssistantPart::ToolCall(
            ToolCall {
                id: call_id("call_get_weather_1"),
                name: tool_name("get_weather"),
                arguments: json!({"city": "Paris"}),
            },
        )])),
        tool_result_message("message_2", "call_get_weather_1", "Cloudy."),
    ]);

    let error = service
        .stream(request, ModelCallContext::default())
        .await
        .err()
        .expect("encoding must fail before any request");
    let ModelError::Config(message) = error else {
        panic!("expected a config error, got {error:?}");
    };
    assert!(
        message.contains("reasoning"),
        "error must explain the missing reasoning content: {message}"
    );
    // 失败发生在建立前：不得发出任何请求。
    assert!(transport.take_requests().is_empty());
}

#[tokio::test]
async fn encode_rejects_tool_result_with_unknown_call_id() {
    // 对话级配对校验：tool 结果引用了对话中不存在的 ToolCallId，编码侧显式失败。
    let transport = Arc::new(RecordedTransport::new([]));
    let service = service_with(&transport);
    let mut request = turn_1_request();
    request.conversation = ConversationSnapshot::new(vec![
        user_message("message_1", "How is the weather in Paris today?"),
        ConversationMessage::Assistant(assistant_message(vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: part_id("reasoning_1"),
                text: "I should call get_weather first.".to_owned(),
            }),
            AssistantPart::ToolCall(ToolCall {
                id: call_id("call_get_weather_1"),
                name: tool_name("get_weather"),
                arguments: json!({"city": "Paris"}),
            }),
        ])),
        tool_result_message("message_2", "call_missing_9", "Cloudy."),
    ]);

    let error = service
        .stream(request, ModelCallContext::default())
        .await
        .err()
        .expect("encoding must fail before any request");
    let ModelError::Config(message) = error else {
        panic!("expected a config error, got {error:?}");
    };
    assert!(
        message.contains("call_missing_9"),
        "error must name the unmatched tool call id: {message}"
    );
    assert!(transport.take_requests().is_empty());
}

#[tokio::test]
async fn incomplete_tool_arguments_fail_with_tool_arguments_terminal() {
    // arguments 分片在 finish_reason 到达时仍拼不成完整 JSON：走既有 ToolArguments 路径。
    const BROKEN_ARGUMENTS_BODY: &str = concat!(
        "data: {\"id\":\"chatcmpl-broken-1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Call the tool.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-broken-1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_get_weather_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-broken-1\",\"model\":\"deepseek-v4-pro\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        BROKEN_ARGUMENTS_BODY,
    ))]));
    let service = service_with(&transport);
    let stream = service
        .stream(turn_1_request(), ModelCallContext::default())
        .await
        .expect("stream established");
    let collected = EventCollector::collect_validated(stream).await;

    let ModelError::ToolArguments(message) = collected.assert_failed() else {
        panic!("expected a tool arguments failure");
    };
    assert!(message.contains("malformed arguments"));
    collected.assert_single_terminal();
}

#[tokio::test]
async fn profile_declares_deepseek_dialect_and_derives_capabilities() {
    let profile = Profile::deepseek();
    assert_eq!(profile.provider, provider_id("deepseek"));
    assert_eq!(profile.protocol, protocol_id("openai.chat_completions"));
    assert_eq!(
        profile.reasoning_content_field.as_deref(),
        Some("reasoning_content")
    );
    // DeepSeek thinking 无强度档位映射（依据见 Profile::deepseek 文档注释）。
    assert_eq!(profile.reasoning_effort_field, None);
    // thinking 模式下 temperature / top_p 不生效（官方文档，见 Profile::deepseek）。
    assert!(!profile.supports_temperature);
    assert!(!profile.supports_top_p);
    assert!(profile.supports_stop);
    assert_eq!(
        profile.max_output_tokens_field.as_deref(),
        Some("max_tokens")
    );
    assert!(profile.tool_calls_require_reasoning);
    assert_eq!(
        profile.cached_input_tokens_field.as_deref(),
        Some("prompt_cache_hit_tokens")
    );

    let transport = Arc::new(RecordedTransport::new([]));
    let service = service_with(&transport);
    assert!(service.capabilities().reasoning);
    assert!(service.capabilities().tool_calls);
    assert!(service.capabilities().streaming);
}

#[test]
fn thinking_mode_ineffective_generation_params_are_rejected() {
    // thinking 模式下 temperature / top_p 不生效；编码侧显式 Config 而不是静默丢弃。
    let profile = Profile::deepseek();
    let mut request = turn_1_request();

    request.generation.temperature = Some(0.5);
    assert!(matches!(
        encode_request(&request, &profile, MODEL),
        Err(ModelError::Config(_))
    ));
    request.generation.temperature = None;

    request.generation.top_p = Some(0.5);
    assert!(matches!(
        encode_request(&request, &profile, MODEL),
        Err(ModelError::Config(_))
    ));
    request.generation.top_p = None;

    // DeepSeek 方言不声明 reasoning effort 字段，规范 effort 配置显式失败。
    request.reasoning = Some(ReasoningConfig {
        effort: Some(ReasoningEffort::High),
    });
    assert!(matches!(
        encode_request(&request, &profile, MODEL),
        Err(ModelError::Config(_))
    ));
    request.reasoning = None;

    // stop 与 max_tokens 未在不生效列表，正常编码。
    request.generation.stop = vec!["END".to_owned()];
    request.generation.max_output_tokens = Some(256);
    let encoded =
        encode_request(&request, &profile, MODEL).expect("stop and max_tokens are supported");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(json["stop"], json!(["END"]));
    assert_eq!(json["max_tokens"], json!(256));
}

#[test]
fn thinking_mode_explicit_tool_choice_is_rejected() {
    // DeepSeek thinking 模式拒绝任何显式 tool_choice（真实 API 400：
    // "Thinking mode does not support this tool_choice"）；编码侧显式 Config
    // 而不是发出必然被拒的请求。
    let profile = Profile::deepseek();
    for choice in [
        ToolChoice::None,
        ToolChoice::Required,
        ToolChoice::Named(tool_name("get_weather")),
    ] {
        let mut request = turn_1_request();
        request.tool_choice = choice;
        assert!(matches!(
            encode_request(&request, &profile, MODEL),
            Err(ModelError::Config(_))
        ));
    }

    // Auto 与线上默认值等价，省略后正常编码。
    let request = turn_1_request();
    let encoded = encode_request(&request, &profile, MODEL).expect("auto tool choice is omitted");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert!(json.get("tool_choice").is_none());
    assert!(json.get("tools").is_some());
}

#[test]
fn flat_cache_usage_maps_only_for_deepseek_profile() {
    let response: ChatResponse = serde_json::from_value(json!({
        "id": "chatcmpl-usage-1",
        "model": "deepseek-v4-pro",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "done"},
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": 128,
            "completion_tokens": 42,
            "total_tokens": 170,
            "prompt_cache_hit_tokens": 64,
            "prompt_cache_miss_tokens": 64,
            "completion_tokens_details": {"reasoning_tokens": 20},
        },
    }))
    .expect("parse response");

    // DeepSeek 方言：扁平的 prompt_cache_hit_tokens 映射进 cached_input_tokens。
    let message = decode_response(&response, &Profile::deepseek()).expect("decode response");
    assert_eq!(
        message
            .usage
            .as_ref()
            .and_then(|usage| usage.cached_input_tokens),
        Some(64)
    );

    // 基础方言只认 OpenAI 嵌套明细；扁平字段不映射，嵌套形状行为不变。
    let base = Profile::openai_compatible(provider_id("openai"));
    let message = decode_response(&response, &base).expect("decode response");
    assert_eq!(
        message
            .usage
            .as_ref()
            .and_then(|usage| usage.cached_input_tokens),
        None
    );
    assert_eq!(
        message
            .usage
            .as_ref()
            .and_then(|usage| usage.reasoning_tokens),
        Some(20)
    );
}

/// 真实 DeepSeek API smoke test：thinking + tool call 两轮往返。
///
/// 默认被 `#[ignore]`，不进入 workspace 默认测试集合；运行方式与 `.env`
/// 模板见 crate 文档（lib.rs）。缺少 `DEEPSEEK_API_KEY` 时打印提示并软跳过。
/// 本测试绝不打印 credential 与请求/响应原文，也绝不把真实响应写入 fixture。
#[tokio::test]
#[ignore = "real DeepSeek API call; requires DEEPSEEK_API_KEY"]
async fn real_api_smoke_thinking_tool_call_round_trip() {
    dotenvy::dotenv().ok();
    let Ok(api_key) = std::env::var("DEEPSEEK_API_KEY") else {
        // 软跳过：缺少凭据不算通过也不算失败。
        eprintln!("DEEPSEEK_API_KEY is not set; skipping the real DeepSeek API smoke test");
        return;
    };
    let base_url = std::env::var("DEEPSEEK_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://api.deepseek.com".to_owned());
    let service = OpenAiCompatibleService::new(
        base_url.clone(),
        BearerCredential::new(api_key),
        "deepseek-v4-flash",
        128_000,
        Profile::deepseek(),
        TransportTimeouts::default(),
    )
    .expect("reqwest transport must build");

    // 第一轮：thinking + tool call。thinking 模式拒绝显式 tool_choice，
    // 不能强制调工具，只能靠 prompt 触发；模型不调工具属于外部行为，
    // 下面按"非协议回归"单独分类。
    let turn_1 = turn_1_request();
    let stream = match service.stream(turn_1, ModelCallContext::default()).await {
        Ok(stream) => stream,
        Err(error) => fail_smoke("turn 1 establishment", &error),
    };
    let collected = EventCollector::collect_validated(stream).await;
    collected.assert_single_terminal();
    if let Some(error) = collected.failure() {
        fail_smoke("turn 1 stream", error);
    }
    let message = collected.assert_finished().clone();
    // 断言宽松（真实 API 非确定）：只钉死 thinking + tool call 的协议形状。
    if !message
        .parts
        .iter()
        .any(|part| matches!(part, AssistantPart::Reasoning(_)))
    {
        panic!("protocol regression: thinking mode turn 1 finished without a reasoning part");
    }
    let Some(tool_call) = message.parts.iter().find_map(|part| match part {
        AssistantPart::ToolCall(call) => Some(call.clone()),
        _ => None,
    }) else {
        panic!(
            "external behavior at turn 1: the model did not call the required tool (not a protocol regression)"
        );
    };

    // 第二轮：AssistantMessage + ToolResult 回传，tool_call_id 必须配对成功。
    let turn_2 = ModelRequest {
        conversation: ConversationSnapshot::new(vec![
            user_message("message_1", "How is the weather in Paris today?"),
            ConversationMessage::Assistant(message),
            tool_result_message(
                "message_2",
                tool_call.id.as_str(),
                "Cloudy, 7 to 13 degrees Celsius.",
            ),
        ]),
        ..turn_1_request()
    };
    let stream = match service.stream(turn_2, ModelCallContext::default()).await {
        Ok(stream) => stream,
        Err(error) => fail_smoke("turn 2 establishment", &error),
    };
    let collected = EventCollector::collect_validated(stream).await;
    collected.assert_single_terminal();
    if let Some(error) = collected.failure() {
        fail_smoke("turn 2 stream", error);
    }
    let message = collected.assert_finished();
    if !message
        .parts
        .iter()
        .any(|part| matches!(part, AssistantPart::Reasoning(_)))
    {
        panic!("protocol regression: thinking mode turn 2 finished without a reasoning part");
    }
    eprintln!("real DeepSeek API smoke test passed: thinking + tool call round trip finished");
}

/// 区分协议回归与外部网络/额度问题（版本设计 11.3 的失败分类要求）。
///
/// 配置/协议/arguments 错误意味着 Adapter 协议回归或测试自身缺陷；
/// 认证、限流、传输和 Provider 拒绝属于外部环境问题，不算协议回归。
fn fail_smoke(stage: &str, error: &ModelError) -> ! {
    match error {
        ModelError::Config(_) | ModelError::Protocol(_) | ModelError::ToolArguments(_) => {
            panic!("protocol regression at {stage}: {error}")
        }
        external => {
            panic!(
                "external network/quota issue at {stage} (not a protocol regression): {external}"
            )
        }
    }
}
