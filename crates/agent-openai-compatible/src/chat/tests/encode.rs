use super::*;

#[test]
fn encode_plain_text_request_uses_streaming_wire() {
    let adapter = base_adapter();
    let mut req = request(vec![user_message("message_1", &["What date is it?"])]);
    req.system = SystemPromptSnapshot::new(vec!["You are brief.".to_owned()]);

    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
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
    let adapter = base_adapter();
    let message = ConversationMessage::User(UserMessage {
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        id: message_id("message_1"),
        parts: vec![
            UserPart::Injected(TextPart {
                id: part_id("injected_1"),
                text: "legacy constraint".to_owned(),
            }),
            UserPart::InternalContext(
                InternalContextPart::new(
                    part_id("internal_1"),
                    "boundary_1",
                    "goal_continuation",
                    "<constraint>answer briefly</constraint>",
                )
                .expect("internal context"),
            ),
            UserPart::Text(TextPart {
                id: part_id("text_1"),
                text: "What date is it?".to_owned(),
            }),
        ],
    });

    let encoded = encode_request(&request(vec![message]), &adapter, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert!(json["messages"][0].get("origin").is_none());
    assert!(json["messages"][0].get("transcript_visibility").is_none());
    // 旧 Injected、新 InternalContext 与 Text 在线上都是文本，多 parts 保序进入数组。
    assert_eq!(
        json["messages"][0]["content"],
        json!([
            {"type": "text", "text": "legacy constraint"},
            {"type": "text", "text": "<constraint>answer briefly</constraint>"},
            {"type": "text", "text": "What date is it?"},
        ])
    );
}

#[test]
fn encode_quoted_text_projects_frozen_context_without_local_identity() {
    let message = ConversationMessage::User(UserMessage {
        origin: Default::default(),
        transcript_visibility: Default::default(),
        id: message_id("message_quote"),
        parts: vec![UserPart::QuotedText(QuotedTextPart {
            quote_id: part_id("local-quote-id"),
            exact: "selected <text>".to_owned(),
            prefix: "before & context".to_owned(),
            suffix: "after context".to_owned(),
            source_owner: QuotedTextSourceOwner::MainSession {
                session_id: "private-session".to_owned(),
            },
            source_generation: 2,
            source_message_id: message_id("private-message"),
            text_start_utf16: 7,
            text_end_utf16: 22,
            source_role: QuotedTextSourceRole::Assistant,
            source_label: "Assistant \"A\"".to_owned(),
            source_created_at_ms: Some(42),
            source_available: true,
        })],
    });

    let encoded = encode_request(&request(vec![message]), &base_adapter(), MODEL)
        .expect("encode quoted text");
    let json = serde_json::to_value(encoded).expect("request JSON");
    let content = json["messages"][0]["content"]
        .as_str()
        .expect("single text content");
    assert!(content.contains("<content format=\"text\">selected &lt;text&gt;</content>"));
    assert!(content.contains("<prefix>before &amp; context</prefix>"));
    assert!(!content.contains("private-session"));
    assert!(!content.contains("private-message"));
    assert!(!content.contains("local-quote-id"));
}

#[test]
fn encode_file_references_preserves_part_and_file_order_with_xml_text_escaping() {
    let adapter = base_adapter();
    let message = ConversationMessage::User(UserMessage {
        origin: Default::default(),
        transcript_visibility: Default::default(),
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

    let encoded = encode_request(&request(vec![message]), &adapter, MODEL).expect("encode request");
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
fn encode_prepared_images_preserves_mixed_order_and_hides_readable_path() {
    let adapter = base_adapter();
    let image_path = "/stable/private/image.png";
    let message = ConversationMessage::User(UserMessage {
        origin: Default::default(),
        transcript_visibility: Default::default(),
        id: message_id("message_image"),
        parts: vec![
            UserPart::Text(TextPart {
                id: part_id("text_image"),
                text: "Compare in order".to_owned(),
            }),
            UserPart::FileReferences(FileReferencesPart {
                id: part_id("files_image"),
                files: vec![
                    FileReference {
                        original_name: "notes.txt".to_owned(),
                        readable_path: "/stable/notes.txt".to_owned(),
                    },
                    FileReference {
                        original_name: "image.png".to_owned(),
                        readable_path: image_path.to_owned(),
                    },
                    FileReference {
                        original_name: "tail.csv".to_owned(),
                        readable_path: "/stable/tail.csv".to_owned(),
                    },
                ],
            }),
        ],
    });
    let mut images = agent_model::PreparedModelImages::default();
    images.insert_file_reference(
        image_path.to_owned(),
        agent_model::PreparedModelImage {
            media_type: "image/jpeg".to_owned(),
            bytes: std::sync::Arc::from([0xff, 0xd8, 0xff]),
        },
    );

    let encoded =
        crate::chat::encode_request_with_images(&request(vec![message]), &images, &adapter, MODEL)
            .expect("encode images");
    let json = serde_json::to_value(encoded).expect("serialize request");
    let content = json["messages"][0]["content"]
        .as_array()
        .expect("multipart content");
    assert_eq!(
        content[0],
        json!({"type": "text", "text": "Compare in order"})
    );
    assert!(
        content[1]["text"]
            .as_str()
            .is_some_and(|value| value.contains("notes.txt"))
    );
    assert_eq!(
        content[2],
        json!({
            "type": "image_url",
            "image_url": {"url": "data:image/jpeg;base64,/9j/"}
        })
    );
    assert!(
        content[3]["text"]
            .as_str()
            .is_some_and(|value| value.contains("tail.csv"))
    );
    assert!(!json.to_string().contains(image_path));
}

#[test]
fn aggregated_tool_images_follow_the_complete_ordered_tool_batch() {
    let adapter = base_adapter()
        .with_tool_image_projection(agent_model::ToolImageProjection::AggregatedUserInput);
    let first = ToolImageReference::new(format!("{}.png", "a".repeat(64)), "image/png")
        .expect("first image");
    let second = ToolImageReference::new(format!("{}.jpg", "b".repeat(64)), "image/jpeg")
        .expect("second image");
    let conversation = vec![
        user_message("message-tool-images", &["read both images"]),
        assistant_message(vec![
            tool_call_part("call_1", "read_image", json!({"path": "a.png"})),
            tool_call_part("call_2", "read_image", json!({"path": "b.jpg"})),
        ]),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool-image-1"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![
                    ToolResultPart::text("first"),
                    ToolResultPart::image(first.clone()),
                ])
                .expect("first result"),
                metadata: None,
            },
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool-image-2"),
            result: ToolResult {
                call_id: call_id("call_2"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![
                    ToolResultPart::image(second.clone()),
                    ToolResultPart::json(json!({"ok": true})),
                    ToolResultPart::image(first.clone()),
                ])
                .expect("second result"),
                metadata: None,
            },
        }),
    ];
    let mut images = agent_model::PreparedModelImages::default();
    images.insert_tool_image(
        first.relative_path().to_owned(),
        agent_model::PreparedModelImage {
            media_type: "image/png".to_owned(),
            bytes: std::sync::Arc::from([1_u8, 2, 3]),
        },
    );
    images.insert_tool_image(
        second.relative_path().to_owned(),
        agent_model::PreparedModelImage {
            media_type: "image/jpeg".to_owned(),
            bytes: std::sync::Arc::from([4_u8, 5, 6]),
        },
    );

    let request = request(conversation);
    assert_eq!(request.conversation.messages.len(), 4);
    let encoded = crate::chat::encode_request_with_images(&request, &images, &adapter, MODEL)
        .expect("aggregate tool images");
    assert_eq!(request.conversation.messages.len(), 4);
    let json = serde_json::to_value(encoded).expect("request JSON");
    let messages = json["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[4]["role"], "user");
    assert_eq!(
        messages[2]["content"],
        "first\n[tool_result_image call_id=\"call_1\" part_index=\"1\" supplied_in_following_batch]"
    );
    assert_eq!(
        messages[3]["content"],
        "[tool_result_image call_id=\"call_2\" part_index=\"0\" supplied_in_following_batch]\n{\"ok\":true}\n[tool_result_image call_id=\"call_2\" part_index=\"2\" supplied_in_following_batch]"
    );
    let envelope = messages[4]["content"].as_array().expect("image envelope");
    assert_eq!(envelope.len(), 6);
    assert_eq!(
        envelope[0]["text"],
        "[tool_result_image call_id=\"call_1\" part_index=\"1\" supplied_in_following_batch]"
    );
    assert_eq!(
        envelope[1]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
    assert_eq!(
        envelope[2]["text"],
        "[tool_result_image call_id=\"call_2\" part_index=\"0\" supplied_in_following_batch]"
    );
    assert_eq!(
        envelope[3]["image_url"]["url"],
        "data:image/jpeg;base64,BAUG"
    );
    assert_eq!(
        envelope[5]["image_url"]["url"],
        "data:image/png;base64,AQID"
    );
}

#[test]
fn tool_images_fail_closed_without_a_verified_projection_or_prepared_resource() {
    let reference =
        ToolImageReference::new(format!("{}.png", "c".repeat(64)), "image/png").expect("image");
    let conversation = vec![
        user_message("message-tool-image", &["read image"]),
        assistant_message(vec![tool_call_part(
            "call_1",
            "read_image",
            json!({"path": "image.png"}),
        )]),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool-image"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![ToolResultPart::image(reference)])
                    .expect("image result"),
                metadata: None,
            },
        }),
    ];
    assert!(matches!(
        crate::chat::encode_request_with_images(
            &request(conversation.clone()),
            &agent_model::PreparedModelImages::default(),
            &base_adapter(),
            MODEL,
        ),
        Err(ModelError::Config(_))
    ));
    assert!(matches!(
        crate::chat::encode_request_with_images(
            &request(conversation),
            &agent_model::PreparedModelImages::default(),
            &base_adapter()
                .with_tool_image_projection(agent_model::ToolImageProjection::AggregatedUserInput,),
            MODEL,
        ),
        Err(ModelError::Resource(_))
    ));
}

#[test]
fn aggregated_projection_keeps_error_results_before_the_single_image_envelope() {
    let reference =
        ToolImageReference::new(format!("{}.png", "d".repeat(64)), "image/png").expect("image");
    let conversation = vec![
        user_message("message-mixed-tool-results", &["read available images"]),
        assistant_message(vec![
            tool_call_part("call_1", "read_image", json!({"path": "ok.png"})),
            tool_call_part("call_2", "read_image", json!({"path": "missing.png"})),
        ]),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool-success"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![ToolResultPart::image(reference.clone())])
                    .expect("success"),
                metadata: None,
            },
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool-error"),
            result: ToolResult {
                call_id: call_id("call_2"),
                status: ToolResultStatus::Error,
                content: ToolResultContent::text("image source is unavailable"),
                metadata: None,
            },
        }),
    ];
    let mut images = agent_model::PreparedModelImages::default();
    images.insert_tool_image(
        reference.relative_path().to_owned(),
        agent_model::PreparedModelImage {
            media_type: "image/jpeg".to_owned(),
            bytes: std::sync::Arc::from([7_u8, 8, 9]),
        },
    );
    let encoded = crate::chat::encode_request_with_images(
        &request(conversation),
        &images,
        &base_adapter()
            .with_tool_image_projection(agent_model::ToolImageProjection::AggregatedUserInput),
        MODEL,
    )
    .expect("mixed batch");
    let json = serde_json::to_value(encoded).expect("request JSON");
    assert_eq!(json["messages"][2]["role"], "tool");
    assert_eq!(json["messages"][3]["role"], "tool");
    assert_eq!(
        json["messages"][3]["content"],
        "image source is unavailable"
    );
    assert_eq!(json["messages"][4]["role"], "user");
    assert_eq!(
        json["messages"][4]["content"]
            .as_array()
            .expect("envelope")
            .len(),
        2
    );
}

#[test]
fn encode_context_summary_uses_a_derived_system_message() {
    let adapter = base_adapter();
    let message = ConversationMessage::ContextSummary(ContextSummaryMessage {
        id: message_id("summary_1"),
        text: "The user selected a local-first architecture.".to_owned(),
        model: None,
        usage: None,
        compacted_usage: None,
    });

    let mut req = request(vec![message]);
    req.system = SystemPromptSnapshot::new(vec![
        "base system".to_owned(),
        "directory system".to_owned(),
    ]);
    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(json["messages"].as_array().expect("messages").len(), 1);
    assert_eq!(
        json["messages"][0],
        json!({
            "role": "system",
            "content": "base system\n\ndirectory system\n\n[Context summary derived from earlier conversation]\nThe user selected a local-first architecture."
        })
    );
}

#[test]
fn encode_merges_leading_system_content_and_preserves_multi_turn_order() {
    let adapter = reasoning_adapter();
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
                content: ToolResultContent::json(json!({"date": "2026-07-20"})),
                metadata: None,
            },
        }),
    ]);
    req.system = SystemPromptSnapshot::new(vec!["system one".to_owned(), "system two".to_owned()]);

    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(
        json["messages"],
        json!([
            {"role": "system", "content": "system one\n\nsystem two\n\ninline system"},
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
fn standard_adapter_omits_reasoning_from_cross_provider_history() {
    let req = request(vec![
        user_message("message_1", &["first"]),
        assistant_message(vec![
            reasoning_part("provider-private reasoning"),
            text_part("portable answer"),
        ]),
        user_message("message_2", &["continue"]),
    ]);

    let encoded = encode_request(&req, &base_adapter(), MODEL)
        .expect("cross-provider history remains encodable");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(json["messages"][1]["content"], json!("portable answer"));
    assert!(json["messages"][1].get("reasoning_content").is_none());
}

#[test]
fn encode_tools_and_tool_choice_follow_wire_shape() {
    let adapter = base_adapter();
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
        let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
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
    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert!(json.get("tool_choice").is_none());
}

#[test]
fn encode_tool_choice_requires_consistent_tools() {
    let adapter = base_adapter();

    // 空工具列表时 Required 与请求矛盾：装配错误必须显式失败，不静默退化。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.tool_choice = ToolChoice::Required;
    let error =
        encode_request(&req, &adapter, MODEL).expect_err("required without tools must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("no tools")));

    // 空工具列表时 Named 同样矛盾。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.tool_choice = ToolChoice::Named(tool_name("get_weather"));
    let error = encode_request(&req, &adapter, MODEL).expect_err("named without tools must fail");
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
        encode_request(&req, &adapter, MODEL).expect_err("named tool missing from tools must fail");
    assert!(
        matches!(error, ModelError::Config(message) if message.contains("not in the request tools"))
    );

    // 空工具列表时 Auto/None 省略是安全语义。
    for choice in [ToolChoice::Auto, ToolChoice::None] {
        let mut req = request(vec![user_message("message_1", &["hi"])]);
        req.tool_choice = choice;
        let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
        let json = serde_json::to_value(&encoded).expect("serialize request");
        assert!(json.get("tool_choice").is_none());
    }
}

#[test]
fn assembler_rejects_chunk_identity_changes() {
    let mut assembler = ChunkAssembler::new(reasoning_adapter());
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
    let adapter = reasoning_adapter();

    // 保留键：经 flatten 平铺会产生重复 JSON 字段。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.provider_options
        .insert("deepseek", json!({"model": "other-model"}))
        .expect("insert reserved key succeeds at construction");
    let error = encode_request(&req, &adapter, MODEL).expect_err("reserved key must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("reserved")));

    // 动态字段冲突：max_output_tokens 已把 max_tokens 写入请求根。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.max_output_tokens = Some(256);
    req.provider_options
        .insert("deepseek", json!({"max_tokens": 128}))
        .expect("insert colliding key succeeds at construction");
    let error =
        encode_request(&req, &adapter, MODEL).expect_err("encoded field collision must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("collides")));

    // 非冲突键仍然正常合并进请求根。
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.provider_options
        .insert("deepseek", json!({"thinking": {"type": "enabled"}}))
        .expect("valid provider options");
    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(json["thinking"], json!({"type": "enabled"}));
}

#[test]
fn encode_generation_reasoning_and_provider_options() {
    let adapter = reasoning_adapter();
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

    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
    let json = serde_json::to_value(&encoded).expect("serialize request");
    assert_eq!(json["temperature"], json!(0.5));
    assert_eq!(json["top_p"], json!(0.25));
    assert_eq!(json["stop"], json!(["END"]));
    // max tokens 与 reasoning effort 的字段名随 ChatProtocolAdapter 变化，平铺在请求根。
    assert_eq!(json["max_tokens"], json!(256));
    assert_eq!(json["reasoning_effort"], json!("high"));
    // 只合并命名空间等于 ChatProtocolAdapter Provider 的私有选项。
    assert_eq!(json["thinking"], json!({"type": "enabled"}));
    assert!(json.get("ignored").is_none());
}

#[test]
fn encode_reasoning_effort_uses_the_compiled_wire_value() {
    let adapter = base_adapter().with_reasoning(
        Some("reasoning_content"),
        Some("thinking_budget"),
        std::collections::BTreeMap::from([
            (ReasoningEffort::Low, json!(4096)),
            (ReasoningEffort::Max, json!(32768)),
        ]),
    );
    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.reasoning = Some(ReasoningConfig {
        effort: Some(ReasoningEffort::Max),
    });

    let json = serde_json::to_value(encode_request(&req, &adapter, MODEL).expect("encode request"))
        .expect("serialize request");
    assert_eq!(json["thinking_budget"], json!(32768));
    assert!(json.get("reasoning_effort").is_none());
}

#[test]
fn encode_omits_reasoning_content_without_adapter_field() {
    let encoded = encode_request(
        &request(vec![assistant_message(vec![reasoning_part("think")])]),
        &base_adapter(),
        MODEL,
    )
    .unwrap();
    let json = serde_json::to_value(encoded).unwrap();
    assert!(json["messages"][0].get("reasoning_content").is_none());
}

#[test]
fn encode_preserves_all_reasoning_when_model_dialect_requires_it() {
    let adapter = ChatProtocolAdapter::openai_compatible(provider_id("dashscope"))
        .with_reasoning(
            Some("reasoning_content"),
            Some("reasoning_effort"),
            std::collections::BTreeMap::new(),
        )
        .with_reasoning_replay(crate::ReasoningReplayPolicy::PreserveAll);
    let encoded = encode_request(
        &request(vec![assistant_message(vec![reasoning_part(
            "historic thought",
        )])]),
        &adapter,
        MODEL,
    )
    .expect("encode request");
    let json = serde_json::to_value(encoded).expect("serialize request");

    assert_eq!(
        json["messages"][0]["reasoning_content"],
        json!("historic thought")
    );
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
        &reasoning_adapter(),
        MODEL,
    );
    assert!(matches!(result, Err(ModelError::Config(_))));
}

#[test]
fn encode_rejects_generation_parameters_adapter_does_not_support() {
    let adapter = limited_adapter();

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.temperature = Some(0.5);
    assert!(matches!(
        encode_request(&req, &adapter, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.top_p = Some(0.5);
    assert!(matches!(
        encode_request(&req, &adapter, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.stop = vec!["END".to_owned()];
    assert!(matches!(
        encode_request(&req, &adapter, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.generation.max_output_tokens = Some(256);
    assert!(matches!(
        encode_request(&req, &adapter, MODEL),
        Err(ModelError::Config(_))
    ));

    let mut req = request(vec![user_message("message_1", &["hi"])]);
    req.reasoning = Some(ReasoningConfig {
        effort: Some(ReasoningEffort::Low),
    });
    assert!(matches!(
        encode_request(&req, &adapter, MODEL),
        Err(ModelError::Config(_))
    ));
}
