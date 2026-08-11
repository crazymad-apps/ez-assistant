use super::*;

#[test]
fn encode_plain_text_request_uses_streaming_wire() {
    let profile = base_profile();
    let mut req = request(vec![user_message("message_1", &["What date is it?"])]);
    req.system = SystemPromptSnapshot::new(vec!["You are brief.".to_owned()]);

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
fn encode_file_references_preserves_part_and_file_order_with_xml_text_escaping() {
    let profile = base_profile();
    let message = ConversationMessage::User(UserMessage {
        id: message_id("message_1"),
        parts: vec![
            UserPart::Text(TextPart {
                id: part_id("text_1"),
                text: "Compare these files".to_owned(),
            }),
            UserPart::FileReferences(FileReferencesPart {
                id: part_id("files_1"),
                files: vec![
                    FileReference {
                        original_name: "a<&>.txt".to_owned(),
                        readable_path: "/stable/a&1.txt".to_owned(),
                    },
                    FileReference {
                        original_name: "b.xlsx".to_owned(),
                        readable_path: "/stable/<b>.xlsx".to_owned(),
                    },
                ],
            }),
            UserPart::Injected(TextPart {
                id: part_id("injected_1"),
                text: "internal continuation".to_owned(),
            }),
        ],
    });

    let encoded = encode_request(&request(vec![message]), &profile, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(
        json["messages"][0]["content"],
        json!([
            {"type": "text", "text": "Compare these files"},
            {
                "type": "text",
                "text": "<attached_files>\n  <file>\n    <name>a&lt;&amp;&gt;.txt</name>\n    <path>/stable/a&amp;1.txt</path>\n  </file>\n  <file>\n    <name>b.xlsx</name>\n    <path>/stable/&lt;b&gt;.xlsx</path>\n  </file>\n</attached_files>"
            },
            {"type": "text", "text": "internal continuation"},
        ])
    );
}

#[test]
fn encode_context_summary_uses_a_derived_system_message() {
    let profile = base_profile();
    let message = ConversationMessage::ContextSummary(ContextSummaryMessage {
        id: message_id("summary_1"),
        text: "The user selected a local-first architecture.".to_owned(),
    });

    let encoded = encode_request(&request(vec![message]), &profile, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(
        json["messages"][0],
        json!({
            "role": "system",
            "content": "[Context summary derived from earlier conversation]\nThe user selected a local-first architecture."
        })
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
    req.system = SystemPromptSnapshot::new(vec!["system one".to_owned(), "system two".to_owned()]);

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
