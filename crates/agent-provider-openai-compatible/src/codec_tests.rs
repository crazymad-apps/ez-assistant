use agent_model::{
    GenerationConfig, ModelError, ModelEvent, ModelRequest, ProviderOptions, ReasoningConfig,
    ReasoningEffort,
};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FinishReason,
    MessageId, ModelIdentity, OpaqueProviderState, PartId, ProtocolId, ProviderId, ReasoningPart,
    SystemMessage, TextPart, TokenUsage, ToolCall, ToolCallId, ToolChoice, ToolDefinition,
    ToolMessage, ToolName, ToolResult, ToolResultContent, ToolResultStatus, UserMessage, UserPart,
};
use serde_json::{Value, json};

use crate::{
    ChatAssistantMessage, ChatChunk, ChatErrorBody, ChatMessage, ChatRequest, ChatResponse,
    ChatStreamOptions, ChunkAssembler, Profile, decode_assistant_message, decode_error_body,
    decode_response, encode_request,
};

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

/// 基础方言：不支持 reasoning。
fn base_profile() -> Profile {
    Profile::openai_compatible(provider_id("openai"))
}

/// 带 reasoning 字段的方言（用字面量构造，DeepSeek 形态；具名构造见 [`Profile::deepseek`]）。
fn reasoning_profile() -> Profile {
    Profile {
        provider: provider_id("deepseek"),
        protocol: protocol_id("openai.chat_completions"),
        reasoning_content_field: Some("reasoning_content".to_owned()),
        reasoning_effort_field: Some("reasoning_effort".to_owned()),
        supports_temperature: true,
        supports_top_p: true,
        supports_stop: true,
        max_tokens_field: Some("max_tokens".to_owned()),
        supports_tool_choice: true,
        tool_calls_require_reasoning: false,
        cached_input_tokens_field: None,
    }
}

/// 所有 generation 参数都不支持的严格方言。
fn limited_profile() -> Profile {
    Profile {
        provider: provider_id("strict"),
        protocol: protocol_id("openai.chat_completions"),
        reasoning_content_field: None,
        reasoning_effort_field: None,
        supports_temperature: false,
        supports_top_p: false,
        supports_stop: false,
        max_tokens_field: None,
        supports_tool_choice: false,
        tool_calls_require_reasoning: false,
        cached_input_tokens_field: None,
    }
}

/// 测试请求的目标模型名；线上模型名由服务构造期绑定，编码时显式传入。
const MODEL: &str = "deepseek-reasoner";

fn request(conversation: Vec<ConversationMessage>) -> ModelRequest {
    ModelRequest {
        system: vec![],
        conversation: ConversationSnapshot::new(conversation),
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: ProviderOptions::new(),
    }
}

fn user_message(id: &str, texts: &[&str]) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        id: message_id(id),
        parts: texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                UserPart::Text(TextPart {
                    id: part_id(&format!("text_{}", index + 1)),
                    text: (*text).to_owned(),
                })
            })
            .collect(),
    })
}

fn assistant_message(parts: Vec<AssistantPart>) -> ConversationMessage {
    ConversationMessage::Assistant(AssistantMessage {
        id: message_id("turn_1"),
        model: ModelIdentity::new(provider_id("deepseek"), "deepseek-reasoner"),
        parts,
        finish_reason: FinishReason::Stop,
        usage: None,
    })
}

fn reasoning_part(text: &str) -> AssistantPart {
    AssistantPart::Reasoning(ReasoningPart {
        id: part_id("reasoning_1"),
        text: text.to_owned(),
    })
}

fn text_part(text: &str) -> AssistantPart {
    AssistantPart::Text(TextPart {
        id: part_id("text_1"),
        text: text.to_owned(),
    })
}

fn tool_call_part(id: &str, name: &str, arguments: Value) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: call_id(id),
        name: tool_name(name),
        arguments,
    })
}

fn model_identity() -> ModelIdentity {
    ModelIdentity::new(provider_id("deepseek"), "deepseek-reasoner")
}

fn chunk(value: Value) -> ChatChunk {
    serde_json::from_value(value).expect("chunk fixture must parse")
}

fn content_chunk(content: &str) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {"content": content}}],
    }))
}

fn reasoning_chunk(text: &str) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {"reasoning_content": text}}],
    }))
}

fn tool_chunk(tool_calls: Value) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {"tool_calls": tool_calls}}],
    }))
}

fn finish_chunk(reason: &str) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
    }))
}

fn usage_only_chunk(usage: Value) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [],
        "usage": usage,
    }))
}

fn feed(
    assembler: &mut ChunkAssembler,
    chunks: Vec<ChatChunk>,
) -> Result<Vec<ModelEvent>, ModelError> {
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(assembler.push_chunk(&chunk)?);
    }
    Ok(events)
}

fn assemble(chunks: Vec<ChatChunk>) -> Result<Vec<ModelEvent>, ModelError> {
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    let mut events = feed(&mut assembler, chunks)?;
    events.extend(assembler.finalize()?);
    Ok(events)
}

#[test]
fn encode_plain_text_request_uses_streaming_wire() {
    let profile = base_profile();
    let mut req = request(vec![user_message("message_1", &["What date is it?"])]);
    req.system = vec!["You are brief.".to_owned()];

    let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
    assert_eq!(encoded.model, "deepseek-reasoner");
    assert!(encoded.stream);
    assert_eq!(
        encoded.stream_options,
        Some(ChatStreamOptions {
            include_usage: true
        })
    );

    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(
        json["messages"],
        json!([
            {"role": "system", "content": "You are brief."},
            {"role": "user", "content": "What date is it?"},
        ])
    );
    // 未设置的字段一律省略。
    assert!(json.get("tools").is_none());
    assert!(json.get("tool_choice").is_none());
    assert!(json.get("temperature").is_none());
    assert!(json.get("top_p").is_none());
    assert!(json.get("stop").is_none());
    assert!(json.get("max_tokens").is_none());
}

#[test]
fn encode_user_message_with_multiple_parts_uses_text_part_array() {
    let profile = base_profile();
    let message = ConversationMessage::User(UserMessage {
        id: message_id("message_1"),
        parts: vec![
            UserPart::Injected(TextPart {
                id: part_id("injected_1"),
                text: "<constraint>answer briefly</constraint>".to_owned(),
            }),
            UserPart::Text(TextPart {
                id: part_id("text_1"),
                text: "What date is it?".to_owned(),
            }),
        ],
    });

    let encoded = encode_request(&request(vec![message]), &profile, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    // Injected 与 Text 在线上都是文本，多 parts 保序进入 text part 数组。
    assert_eq!(
        json["messages"][0]["content"],
        json!([
            {"type": "text", "text": "<constraint>answer briefly</constraint>"},
            {"type": "text", "text": "What date is it?"},
        ])
    );
}

#[test]
fn encode_system_and_multi_turn_conversation_preserves_order() {
    let profile = reasoning_profile();
    let mut req = request(vec![
        ConversationMessage::System(SystemMessage {
            id: message_id("message_0"),
            text: "inline system".to_owned(),
        }),
        user_message("message_1", &["first", "second"]),
        assistant_message(vec![
            reasoning_part("think"),
            text_part("answer"),
            tool_call_part("call_1", "get_date", json!({})),
        ]),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("message_2"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Json(json!({"date": "2026-07-20"})),
            },
        }),
    ]);
    req.system = vec!["system one".to_owned(), "system two".to_owned()];

    let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(
        json["messages"],
        json!([
            {"role": "system", "content": "system one"},
            {"role": "system", "content": "system two"},
            {"role": "system", "content": "inline system"},
            {"role": "user", "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"},
            ]},
            {"role": "assistant", "content": "answer", "reasoning_content": "think", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "get_date", "arguments": "{}"}},
            ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "{\"date\":\"2026-07-20\"}"},
        ])
    );
}

#[test]
fn encode_tools_and_tool_choice_follow_wire_shape() {
    let profile = base_profile();
    let tool = || ToolDefinition {
        name: tool_name("get_weather"),
        description: "Get the weather.".to_owned(),
        input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
    };
    let cases = [
        (ToolChoice::None, json!("none")),
        (ToolChoice::Required, json!("required")),
        (
            ToolChoice::Named(tool_name("get_weather")),
            json!({"type": "function", "function": {"name": "get_weather"}}),
        ),
    ];
    for (choice, expected) in cases {
        let mut req = request(vec![user_message("message_1", &["hi"])]);
        req.tools = vec![tool()];
        req.tool_choice = choice;
        let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
        let json = serde_json::to_value(&encoded).expect("serialize request");
        assert_eq!(
            json["tools"],
            json!([
                {"type": "function", "function": {
                    "name": "get_weather",
                    "description": "Get the weather.",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
                }},
            ])
        );
        assert_eq!(json["tool_choice"], expected);
    }

    // Auto 是线上的默认值，统一省略（省略与显式传 "auto" 语义相同，且兼容
    // 拒绝显式 tool_choice 的 Provider）。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.tools = vec![tool()];
    req.tool_choice = ToolChoice::Auto;
    let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert!(json.get("tool_choice").is_none());
}

#[test]
fn encode_tool_choice_requires_consistent_tools() {
    let profile = base_profile();

    // 空工具列表时 Required 与请求矛盾：装配错误必须显式失败，不静默退化。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.tool_choice = ToolChoice::Required;
    let error =
        encode_request(&req, &profile, MODEL).expect_err("required without tools must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("no tools")));

    // 空工具列表时 Named 同样矛盾。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.tool_choice = ToolChoice::Named(tool_name("get_weather"));
    let error = encode_request(&req, &profile, MODEL).expect_err("named without tools must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("no tools")));

    // 工具列表非空但 Named 指向不存在的名称。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.tools = vec![ToolDefinition {
        name: tool_name("get_weather"),
        description: "Get the weather.".to_owned(),
        input_schema: json!({"type": "object"}),
    }];
    req.tool_choice = ToolChoice::Named(tool_name("get_date"));
    let error =
        encode_request(&req, &profile, MODEL).expect_err("named tool missing from tools must fail");
    assert!(
        matches!(error, ModelError::Config(message) if message.contains("not in the request tools"))
    );

    // 空工具列表时 Auto/None 省略是安全语义。
    for choice in [ToolChoice::Auto, ToolChoice::None] {
        let mut req = request(vec![user_message("message_1", &["hi"])]);
        req.tool_choice = choice;
        let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
        let json = serde_json::to_value(&encoded).expect("serialize request");
        assert!(json.get("tool_choice").is_none());
    }
}

#[test]
fn assembler_rejects_chunk_identity_changes() {
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    assembler
        .push_chunk(&content_chunk("hello"))
        .expect("first chunk establishes identity");

    // 响应 ID 中途变化。
    let changed_id = chunk(json!({
        "id": "chatcmpl_2",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {"content": "world"}}],
    }));
    let error = assembler
        .push_chunk(&changed_id)
        .expect_err("id change must fail");
    assert!(matches!(error, ModelError::Protocol(message) if message.contains("identity changed")));

    // 模型中途变化。
    let changed_model = chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-chat",
        "choices": [{"index": 0, "delta": {"content": "world"}}],
    }));
    let error = assembler
        .push_chunk(&changed_model)
        .expect_err("model change must fail");
    assert!(matches!(error, ModelError::Protocol(message) if message.contains("identity changed")));

    // choice 序号中途变化。
    let changed_index = chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 1, "delta": {"content": "world"}}],
    }));
    let error = assembler
        .push_chunk(&changed_index)
        .expect_err("choice index change must fail");
    assert!(
        matches!(error, ModelError::Protocol(message) if message.contains("choice index changed"))
    );

    // 校验失败不污染状态：身份一致的 usage-only chunk（空 choices）仍然合法。
    let usage_only = chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }));
    let events = assembler
        .push_chunk(&usage_only)
        .expect("usage chunk with matching identity");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelEvent::UsageUpdated { .. }))
    );
}

#[test]
fn encode_provider_options_reject_colliding_and_reserved_keys() {
    let profile = reasoning_profile();

    // 保留键：经 flatten 平铺会产生重复 JSON 字段。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.provider_options
        .insert("deepseek", json!({"model": "other-model"}))
        .expect("insert reserved key succeeds at construction");
    let error = encode_request(&req, &profile, MODEL).expect_err("reserved key must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("reserved")));

    // 动态字段冲突：max_output_tokens 已把 max_tokens 写入请求根。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.max_output_tokens = Some(256);
    req.provider_options
        .insert("deepseek", json!({"max_tokens": 128}))
        .expect("insert colliding key succeeds at construction");
    let error =
        encode_request(&req, &profile, MODEL).expect_err("encoded field collision must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("collides")));

    // 非冲突键仍然正常合并进请求根。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.provider_options
        .insert("deepseek", json!({"thinking": {"type": "enabled"}}))
        .expect("valid provider options");
    let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(json["thinking"], json!({"type": "enabled"}));
}

#[test]
fn encode_generation_reasoning_and_provider_options() {
    let profile = reasoning_profile();
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation = GenerationConfig {
        temperature: Some(0.5),
        top_p: Some(0.25),
        max_output_tokens: Some(256),
        stop: vec!["END".to_owned()],
    };
    req.reasoning = Some(ReasoningConfig {
        effort: Some(ReasoningEffort::High),
    });
    let mut options = ProviderOptions::new();
    options
        .insert("deepseek", json!({"thinking": {"type": "enabled"}}))
        .expect("valid provider options");
    options
        .insert("openai", json!({"ignored": true}))
        .expect("valid provider options");
    req.provider_options = options;

    let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(json["temperature"], json!(0.5));
    assert_eq!(json["top_p"], json!(0.25));
    assert_eq!(json["stop"], json!(["END"]));
    // max tokens 与 reasoning effort 的字段名随 Profile 变化，平铺在请求根。
    assert_eq!(json["max_tokens"], json!(256));
    assert_eq!(json["reasoning_effort"], json!("high"));
    // 只合并命名空间等于 Profile Provider 的私有选项。
    assert_eq!(json["thinking"], json!({"type": "enabled"}));
    assert!(json.get("ignored").is_none());
}

#[test]
fn encode_rejects_reasoning_content_without_profile_field() {
    let result = encode_request(
        &request(vec![assistant_message(vec![reasoning_part("think")])]),
        &base_profile(),
        MODEL,
    );
    assert!(matches!(result, Err(ModelError::Config(_))));
}

#[test]
fn encode_rejects_unmappable_provider_state() {
    let state = OpaqueProviderState::new(
        provider_id("openai"),
        protocol_id("responses"),
        "encrypted_reasoning",
        "application/json",
        1,
        vec![1, 2, 3],
    )
    .expect("valid provider state");
    let result = encode_request(
        &request(vec![assistant_message(vec![AssistantPart::ProviderState(
            state,
        )])]),
        &reasoning_profile(),
        MODEL,
    );
    assert!(matches!(result, Err(ModelError::Config(_))));
}

#[test]
fn encode_rejects_generation_parameters_profile_does_not_support() {
    let profile = limited_profile();

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.temperature = Some(0.5);
    assert!(matches!(
        encode_request(&req, &profile, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.top_p = Some(0.5);
    assert!(matches!(
        encode_request(&req, &profile, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.stop = vec!["END".to_owned()];
    assert!(matches!(
        encode_request(&req, &profile, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.max_output_tokens = Some(256);
    assert!(matches!(
        encode_request(&req, &profile, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.reasoning = Some(ReasoningConfig {
        effort: Some(ReasoningEffort::Low),
    });
    assert!(matches!(
        encode_request(&req, &profile, MODEL),
        Err(ModelError::Config(_))
    ));
}

#[test]
fn decode_response_maps_message_usage_and_finish_reason() {
    let response: ChatResponse = serde_json::from_value(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "reasoning_content": "think", "content": "answer"},
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15,
            "prompt_tokens_details": {"cached_tokens": 4},
            "completion_tokens_details": {"reasoning_tokens": 3},
        },
    }))
    .expect("parse response");

    let message = decode_response(&response, &reasoning_profile()).expect("decode response");
    assert_eq!(message.id, message_id("chatcmpl_1"));
    assert_eq!(message.model, model_identity());
    assert_eq!(
        message.parts,
        vec![
            AssistantPart::Reasoning(ReasoningPart {
                id: part_id("part_1"),
                text: "think".to_owned(),
            }),
            AssistantPart::Text(TextPart {
                id: part_id("part_2"),
                text: "answer".to_owned(),
            }),
        ]
    );
    assert_eq!(message.finish_reason, FinishReason::Stop);
    assert_eq!(
        message.usage,
        Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cached_input_tokens: Some(4),
            reasoning_tokens: Some(3),
        })
    );
}

#[test]
fn decode_response_parses_tool_call_arguments() {
    let response: ChatResponse = serde_json::from_value(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "get_date", "arguments": ""}},
                ],
            },
            "finish_reason": "tool_calls",
        }],
    }))
    .expect("parse response");

    let message = decode_response(&response, &reasoning_profile()).expect("decode response");
    assert_eq!(
        message.parts,
        vec![
            AssistantPart::ToolCall(ToolCall {
                id: call_id("call_1"),
                name: tool_name("get_weather"),
                arguments: json!({"city": "Paris"}),
            }),
            // 空 arguments 按空对象处理。
            AssistantPart::ToolCall(ToolCall {
                id: call_id("call_2"),
                name: tool_name("get_date"),
                arguments: json!({}),
            }),
        ]
    );
    assert_eq!(message.finish_reason, FinishReason::ToolCalls);
}

#[test]
fn decode_response_rejects_missing_choice_or_finish_reason() {
    let response: ChatResponse =
        serde_json::from_value(json!({"id": "chatcmpl_1", "model": "m", "choices": []}))
            .expect("parse response");
    assert!(matches!(
        decode_response(&response, &reasoning_profile()),
        Err(ModelError::Protocol(_))
    ));

    let response: ChatResponse = serde_json::from_value(json!({
        "id": "chatcmpl_1",
        "model": "m",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "x"}}],
    }))
    .expect("parse response");
    assert!(matches!(
        decode_response(&response, &reasoning_profile()),
        Err(ModelError::Protocol(_))
    ));
}

#[test]
fn decode_error_body_maps_type_and_code_strings() {
    let body: ChatErrorBody = serde_json::from_value(json!({
        "error": {"message": "bad key", "type": "authentication_error", "code": "invalid_api_key"},
    }))
    .expect("parse error body");
    assert_eq!(
        decode_error_body(&body),
        ModelError::Auth("bad key".to_owned())
    );

    let body: ChatErrorBody = serde_json::from_value(json!({
        "error": {"message": "slow down", "type": "requests", "code": "rate_limit_exceeded"},
    }))
    .expect("parse error body");
    assert_eq!(
        decode_error_body(&body),
        ModelError::RateLimited("slow down".to_owned())
    );

    let body: ChatErrorBody = serde_json::from_value(json!({
        "error": {"message": "oops", "type": "server_error"},
    }))
    .expect("parse error body");
    assert_eq!(
        decode_error_body(&body),
        ModelError::Provider {
            message: "oops".to_owned(),
            status: None,
        }
    );
}

#[test]
fn stream_plain_text_turn_produces_full_event_sequence() {
    let events = assemble(vec![
        content_chunk("Hello"),
        content_chunk(", world"),
        finish_chunk("stop"),
        usage_only_chunk(json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15})),
    ])
    .expect("assemble events");
    let usage = TokenUsage {
        input_tokens: 10,
        output_tokens: 5,
        total_tokens: 15,
        cached_input_tokens: None,
        reasoning_tokens: None,
    };
    assert_eq!(
        events,
        vec![
            ModelEvent::TurnStarted {
                message_id: message_id("chatcmpl_1"),
                model: model_identity(),
            },
            ModelEvent::TextStarted {
                id: part_id("part_1"),
            },
            ModelEvent::TextDelta {
                id: part_id("part_1"),
                delta: "Hello".to_owned(),
            },
            ModelEvent::TextDelta {
                id: part_id("part_1"),
                delta: ", world".to_owned(),
            },
            ModelEvent::TextFinished {
                id: part_id("part_1"),
            },
            ModelEvent::UsageUpdated {
                usage: usage.clone(),
            },
            ModelEvent::TurnFinished {
                message: AssistantMessage {
                    id: message_id("chatcmpl_1"),
                    model: model_identity(),
                    parts: vec![AssistantPart::Text(TextPart {
                        id: part_id("part_1"),
                        text: "Hello, world".to_owned(),
                    })],
                    finish_reason: FinishReason::Stop,
                    usage: Some(usage),
                },
            },
        ]
    );
}

#[test]
fn stream_interleaved_reasoning_and_text_switch_parts() {
    let events = assemble(vec![
        reasoning_chunk("A"),
        reasoning_chunk("B"),
        content_chunk("X"),
        reasoning_chunk("C"),
        finish_chunk("stop"),
    ])
    .expect("assemble events");
    assert_eq!(
        events,
        vec![
            ModelEvent::TurnStarted {
                message_id: message_id("chatcmpl_1"),
                model: model_identity(),
            },
            ModelEvent::ReasoningStarted {
                id: part_id("part_1"),
            },
            ModelEvent::ReasoningDelta {
                id: part_id("part_1"),
                delta: "A".to_owned(),
            },
            ModelEvent::ReasoningDelta {
                id: part_id("part_1"),
                delta: "B".to_owned(),
            },
            ModelEvent::ReasoningFinished {
                id: part_id("part_1"),
            },
            ModelEvent::TextStarted {
                id: part_id("part_2"),
            },
            ModelEvent::TextDelta {
                id: part_id("part_2"),
                delta: "X".to_owned(),
            },
            ModelEvent::TextFinished {
                id: part_id("part_2"),
            },
            ModelEvent::ReasoningStarted {
                id: part_id("part_3"),
            },
            ModelEvent::ReasoningDelta {
                id: part_id("part_3"),
                delta: "C".to_owned(),
            },
            ModelEvent::ReasoningFinished {
                id: part_id("part_3"),
            },
            ModelEvent::TurnFinished {
                message: AssistantMessage {
                    id: message_id("chatcmpl_1"),
                    model: model_identity(),
                    // reasoning/text 交错按 Provider 输出顺序排列。
                    parts: vec![
                        AssistantPart::Reasoning(ReasoningPart {
                            id: part_id("part_1"),
                            text: "AB".to_owned(),
                        }),
                        AssistantPart::Text(TextPart {
                            id: part_id("part_2"),
                            text: "X".to_owned(),
                        }),
                        AssistantPart::Reasoning(ReasoningPart {
                            id: part_id("part_3"),
                            text: "C".to_owned(),
                        }),
                    ],
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            },
        ]
    );
}

#[test]
fn stream_and_message_treat_null_reasoning_field_as_absent() {
    // 真实 DeepSeek 流会在没有 reasoning 增量的 chunk 里显式下发
    // "reasoning_content": null；null 等价于字段缺席，不算协议违反。
    let events = assemble(vec![
        chunk(json!({
            "id": "chatcmpl_1",
            "model": "deepseek-reasoner",
            "choices": [{"index": 0, "delta": {"reasoning_content": null}}],
        })),
        reasoning_chunk("A"),
        chunk(json!({
            "id": "chatcmpl_1",
            "model": "deepseek-reasoner",
            "choices": [{"index": 0, "delta": {"reasoning_content": null, "content": "X"}}],
        })),
        finish_chunk("stop"),
    ])
    .expect("null reasoning field is tolerated");
    assert_eq!(
        events,
        vec![
            ModelEvent::TurnStarted {
                message_id: message_id("chatcmpl_1"),
                model: model_identity(),
            },
            ModelEvent::ReasoningStarted {
                id: part_id("part_1"),
            },
            ModelEvent::ReasoningDelta {
                id: part_id("part_1"),
                delta: "A".to_owned(),
            },
            ModelEvent::ReasoningFinished {
                id: part_id("part_1"),
            },
            ModelEvent::TextStarted {
                id: part_id("part_2"),
            },
            ModelEvent::TextDelta {
                id: part_id("part_2"),
                delta: "X".to_owned(),
            },
            ModelEvent::TextFinished {
                id: part_id("part_2"),
            },
            ModelEvent::TurnFinished {
                message: AssistantMessage {
                    id: message_id("chatcmpl_1"),
                    model: model_identity(),
                    parts: vec![
                        AssistantPart::Reasoning(ReasoningPart {
                            id: part_id("part_1"),
                            text: "A".to_owned(),
                        }),
                        AssistantPart::Text(TextPart {
                            id: part_id("part_2"),
                            text: "X".to_owned(),
                        }),
                    ],
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            },
        ]
    );

    // 非流式消息同样：reasoning_content 为 null 时不产出 reasoning part。
    let message: ChatAssistantMessage = serde_json::from_value(json!({
        "role": "assistant",
        "reasoning_content": null,
        "content": "answer",
    }))
    .expect("deserialize assistant message");
    let parts = decode_assistant_message(&message, &reasoning_profile())
        .expect("null reasoning field is tolerated");
    assert_eq!(
        parts,
        vec![AssistantPart::Text(TextPart {
            id: part_id("part_1"),
            text: "answer".to_owned(),
        })]
    );
}

#[test]
fn stream_single_tool_call_assembles_arguments_across_chunks() {
    let events = assemble(vec![
        tool_chunk(json!([{"index": 0, "id": "call_1", "function": {"name": "get_weather"}}])),
        tool_chunk(json!([{"index": 0, "function": {"arguments": "{\"city\":"}}])),
        tool_chunk(json!([{"index": 0, "function": {"arguments": "\"Paris\"}"}}])),
        finish_chunk("tool_calls"),
    ])
    .expect("assemble events");
    assert_eq!(
        events,
        vec![
            ModelEvent::TurnStarted {
                message_id: message_id("chatcmpl_1"),
                model: model_identity(),
            },
            ModelEvent::ToolCallStarted {
                id: call_id("call_1"),
                name: tool_name("get_weather"),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_1"),
                arguments_delta: "{\"city\":".to_owned(),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_1"),
                arguments_delta: "\"Paris\"}".to_owned(),
            },
            ModelEvent::ToolCallFinished {
                id: call_id("call_1"),
                arguments: json!({"city": "Paris"}),
            },
            ModelEvent::TurnFinished {
                message: AssistantMessage {
                    id: message_id("chatcmpl_1"),
                    model: model_identity(),
                    parts: vec![AssistantPart::ToolCall(ToolCall {
                        id: call_id("call_1"),
                        name: tool_name("get_weather"),
                        arguments: json!({"city": "Paris"}),
                    })],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            },
        ]
    );
}

#[test]
fn stream_buffers_arguments_until_call_id_and_name_arrive() {
    let events = assemble(vec![
        // arguments 片段先于 id/name 到达：先缓冲，不产出事件。
        tool_chunk(json!([{"index": 0, "function": {"arguments": "{\"a\":"}}])),
        tool_chunk(json!([{"index": 0, "id": "call_1", "function": {"name": "get_answer"}}])),
        tool_chunk(json!([{"index": 0, "function": {"arguments": "1}"}}])),
        finish_chunk("tool_calls"),
    ])
    .expect("assemble events");
    assert_eq!(
        events,
        vec![
            ModelEvent::TurnStarted {
                message_id: message_id("chatcmpl_1"),
                model: model_identity(),
            },
            ModelEvent::ToolCallStarted {
                id: call_id("call_1"),
                name: tool_name("get_answer"),
            },
            // started 之后按序补发缓冲的片段。
            ModelEvent::ToolCallDelta {
                id: call_id("call_1"),
                arguments_delta: "{\"a\":".to_owned(),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_1"),
                arguments_delta: "1}".to_owned(),
            },
            ModelEvent::ToolCallFinished {
                id: call_id("call_1"),
                arguments: json!({"a": 1}),
            },
            ModelEvent::TurnFinished {
                message: AssistantMessage {
                    id: message_id("chatcmpl_1"),
                    model: model_identity(),
                    parts: vec![AssistantPart::ToolCall(ToolCall {
                        id: call_id("call_1"),
                        name: tool_name("get_answer"),
                        arguments: json!({"a": 1}),
                    })],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            },
        ]
    );
}

#[test]
fn stream_parallel_tool_calls_complete_in_index_order() {
    let events = assemble(vec![
        tool_chunk(json!([{"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}])),
        // 出现更大的新 index：比它小的未完成 index 先收尾。
        tool_chunk(json!([{"index": 1, "id": "call_2", "function": {"name": "get_time", "arguments": "{\"tz\":\"UTC\"}"}}])),
        finish_chunk("tool_calls"),
    ])
    .expect("assemble events");
    assert_eq!(
        events,
        vec![
            ModelEvent::TurnStarted {
                message_id: message_id("chatcmpl_1"),
                model: model_identity(),
            },
            ModelEvent::ToolCallStarted {
                id: call_id("call_1"),
                name: tool_name("get_weather"),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_1"),
                arguments_delta: "{\"city\":\"Paris\"}".to_owned(),
            },
            ModelEvent::ToolCallFinished {
                id: call_id("call_1"),
                arguments: json!({"city": "Paris"}),
            },
            ModelEvent::ToolCallStarted {
                id: call_id("call_2"),
                name: tool_name("get_time"),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_2"),
                arguments_delta: "{\"tz\":\"UTC\"}".to_owned(),
            },
            ModelEvent::ToolCallFinished {
                id: call_id("call_2"),
                arguments: json!({"tz": "UTC"}),
            },
            ModelEvent::TurnFinished {
                message: AssistantMessage {
                    id: message_id("chatcmpl_1"),
                    model: model_identity(),
                    // tool calls 按 index 序进入最终消息。
                    parts: vec![
                        AssistantPart::ToolCall(ToolCall {
                            id: call_id("call_1"),
                            name: tool_name("get_weather"),
                            arguments: json!({"city": "Paris"}),
                        }),
                        AssistantPart::ToolCall(ToolCall {
                            id: call_id("call_2"),
                            name: tool_name("get_time"),
                            arguments: json!({"tz": "UTC"}),
                        }),
                    ],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            },
        ]
    );
}

#[test]
fn stream_tool_call_without_arguments_defaults_to_empty_object() {
    let events = assemble(vec![
        tool_chunk(json!([{"index": 0, "id": "call_1", "function": {"name": "get_date"}}])),
        finish_chunk("tool_calls"),
    ])
    .expect("assemble events");
    assert!(
        events.iter().any(|event| matches!(
            event,
            ModelEvent::ToolCallFinished { arguments, .. } if *arguments == json!({})
        )),
        "empty arguments must assemble into an empty object: {events:?}"
    );
}

#[test]
fn stream_maps_all_finish_reasons() {
    let cases = [
        ("stop", FinishReason::Stop),
        ("tool_calls", FinishReason::ToolCalls),
        ("length", FinishReason::Length),
        ("content_filter", FinishReason::ContentFilter),
        (
            "function_call",
            FinishReason::Other("function_call".to_owned()),
        ),
    ];
    for (raw, expected) in cases {
        let events = assemble(vec![finish_chunk(raw)]).expect("assemble events");
        let Some(ModelEvent::TurnFinished { message }) = events.last() else {
            panic!("stream must end with TurnFinished");
        };
        assert_eq!(message.finish_reason, expected, "finish reason `{raw}`");
    }
}

#[test]
fn stream_maps_usage_details_into_token_usage() {
    let events = assemble(vec![
        content_chunk("Hi"),
        finish_chunk("stop"),
        usage_only_chunk(json!({
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15,
            "prompt_tokens_details": {"cached_tokens": 4},
            "completion_tokens_details": {"reasoning_tokens": 3},
        })),
    ])
    .expect("assemble events");
    assert!(
        events.iter().any(|event| matches!(
            event,
            ModelEvent::UsageUpdated { usage }
                if usage.cached_input_tokens == Some(4) && usage.reasoning_tokens == Some(3)
        )),
        "usage details must surface in UsageUpdated: {events:?}"
    );
    let Some(ModelEvent::TurnFinished { message }) = events.last() else {
        panic!("stream must end with TurnFinished");
    };
    assert_eq!(
        message.usage,
        Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cached_input_tokens: Some(4),
            reasoning_tokens: Some(3),
        })
    );
}

#[test]
fn stream_rejects_delta_for_completed_tool_index() {
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    feed(
        &mut assembler,
        vec![
            tool_chunk(json!([{"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{}"}}])),
            // index 1 的出现把 index 0 收尾。
            tool_chunk(json!([{"index": 1, "id": "call_2", "function": {"name": "get_time"}}])),
        ],
    )
    .expect("first chunks assemble");
    let result = assembler.push_chunk(&tool_chunk(
        json!([{"index": 0, "function": {"arguments": "{}"}}]),
    ));
    assert!(matches!(result, Err(ModelError::Protocol(_))));
}

#[test]
fn stream_rejects_duplicate_call_id_across_indexes() {
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    feed(
        &mut assembler,
        vec![tool_chunk(
            json!([{"index": 0, "id": "call_1", "function": {"name": "get_weather"}}]),
        )],
    )
    .expect("first chunk assembles");
    let result = assembler.push_chunk(&tool_chunk(
        json!([{"index": 1, "id": "call_1", "function": {"name": "get_time"}}]),
    ));
    assert!(matches!(result, Err(ModelError::Protocol(_))));
}

#[test]
fn stream_rejects_malformed_tool_arguments_and_malformed_chunk_json() {
    let result = assemble(vec![
        tool_chunk(
            json!([{"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{broken"}}]),
        ),
        finish_chunk("tool_calls"),
    ]);
    assert!(matches!(result, Err(ModelError::ToolArguments(_))));

    // 畸形 JSON 在 schema 解析阶段就失败。
    assert!(serde_json::from_str::<ChatChunk>("{\"id\":").is_err());
}

#[test]
fn stream_ignores_unknown_compatible_fields() {
    let unknown = chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "object": "chat.completion.chunk",
        "created": 1_700_000_000,
        "system_fingerprint": "fp_1",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": "Hi", "logprobs": null},
            "logprobs": null,
        }],
    }));
    let events = assemble(vec![unknown, finish_chunk("stop")]).expect("assemble events");
    assert!(matches!(
        events.last(),
        Some(ModelEvent::TurnFinished { .. })
    ));
}

#[test]
fn stream_finalize_requires_chunks_and_finish_reason() {
    // 没见过任何 chunk。
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    assert!(matches!(assembler.finalize(), Err(ModelError::Protocol(_))));

    // 没有 finish_reason 就结束：严格语义下属于协议违例。
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    feed(&mut assembler, vec![content_chunk("Hi")]).expect("chunk assembles");
    assert!(matches!(assembler.finalize(), Err(ModelError::Protocol(_))));
}

#[test]
fn stream_rejects_choice_chunk_after_finish_but_accepts_usage() {
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    feed(&mut assembler, vec![finish_chunk("stop")]).expect("finish assembles");
    let result = assembler.push_chunk(&content_chunk("late"));
    assert!(matches!(result, Err(ModelError::Protocol(_))));

    // finish_reason 之后的空 choices usage chunk 是合法的流尾形态。
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    feed(&mut assembler, vec![finish_chunk("stop")]).expect("finish assembles");
    let events = assembler
        .push_chunk(&usage_only_chunk(
            json!({"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}),
        ))
        .expect("usage chunk assembles");
    assert!(matches!(
        events.as_slice(),
        [ModelEvent::UsageUpdated { .. }]
    ));
}

#[test]
fn tool_call_id_round_trips_from_call_to_result() {
    let profile = reasoning_profile();
    let req = request(vec![
        assistant_message(vec![tool_call_part("call_1", "get_date", json!({}))]),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("message_2"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::Text("2026-07-20".to_owned()),
            },
        }),
    ]);
    let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
    let wire = serde_json::to_value(&encoded).expect("serialize request");
    // tool result 回填对应 tool call 的 ID。
    assert_eq!(wire["messages"][0]["tool_calls"][0]["id"], json!("call_1"));
    assert_eq!(
        wire["messages"][1],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "2026-07-20"})
    );

    // 回读：原生 assistant 消息解码后仍携带同一 call id。
    let readback: ChatRequest = serde_json::from_value(wire).expect("deserialize request");
    let ChatMessage::Assistant(assistant) = &readback.messages[0] else {
        panic!("expected assistant message");
    };
    let parts = decode_assistant_message(assistant, &profile).expect("decode assistant message");
    assert_eq!(
        parts,
        vec![AssistantPart::ToolCall(ToolCall {
            id: call_id("call_1"),
            name: tool_name("get_date"),
            arguments: json!({}),
        })]
    );
}

#[test]
fn canonical_native_canonical_round_trip_preserves_assistant_parts() {
    let profile = reasoning_profile();
    let calls = vec![
        ToolCall {
            id: call_id("call_1"),
            name: tool_name("get_weather"),
            arguments: json!({"city": "Paris"}),
        },
        ToolCall {
            id: call_id("call_2"),
            name: tool_name("get_time"),
            arguments: json!({"tz": "UTC"}),
        },
    ];
    let req = request(vec![
        user_message("message_1", &["weather?"]),
        assistant_message(vec![
            reasoning_part("think "),
            text_part("let me "),
            reasoning_part("more"),
            text_part("check"),
            AssistantPart::ToolCall(calls[0].clone()),
            AssistantPart::ToolCall(calls[1].clone()),
        ]),
    ]);

    let encoded = encode_request(&req, &profile, MODEL).expect("encode request");
    let wire = serde_json::to_string(&encoded).expect("serialize request");
    let readback: ChatRequest = serde_json::from_str(&wire).expect("deserialize request");
    let ChatMessage::Assistant(assistant) = &readback.messages[1] else {
        panic!("expected assistant message");
    };
    // 线上形态：reasoning/text 各自拼接，tool calls 完整保留。
    assert_eq!(
        assistant.extra.get("reasoning_content"),
        Some(&json!("think more"))
    );
    assert!(!assistant.extra.contains_key("role"));
    assert_eq!(assistant.content.as_deref(), Some("let me check"));

    let parts = decode_assistant_message(assistant, &profile).expect("decode assistant message");
    let reasoning: String = parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Reasoning(part) => Some(part.text.as_str()),
            _ => None,
        })
        .collect();
    let text: String = parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(part) => Some(part.text.as_str()),
            _ => None,
        })
        .collect();
    let decoded_calls: Vec<ToolCall> = parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "think more");
    assert_eq!(text, "let me check");
    assert_eq!(decoded_calls, calls);
}
