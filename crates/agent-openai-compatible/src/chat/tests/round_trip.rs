use super::*;

#[test]
fn tool_call_id_round_trips_from_call_to_result() {
    let adapter = reasoning_adapter();
    let req = request(vec![
        assistant_message(vec![tool_call_part("call_1", "get_date", json!({}))]),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("message_2"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("2026-07-20".to_owned()),
                metadata: None,
            },
        }),
    ]);
    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
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
    let parts = decode_assistant_message(assistant, &adapter).expect("decode assistant message");
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
fn encode_reuses_strict_bidirectional_tool_exchange_validation() {
    let adapter = reasoning_adapter();
    let missing = request(vec![assistant_message(vec![tool_call_part(
        "call_1",
        "get_date",
        json!({}),
    )])]);
    let error = encode_request(&missing, &adapter, MODEL).expect_err("missing result must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("call_1")));

    let out_of_order = request(vec![
        assistant_message(vec![
            tool_call_part("call_1", "get_date", json!({})),
            tool_call_part("call_2", "get_date", json!({})),
        ]),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_2"),
            result: ToolResult {
                call_id: call_id("call_2"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("ok".to_owned()),
                metadata: None,
            },
        }),
    ]);
    let error = encode_request(&out_of_order, &adapter, MODEL).expect_err("out of order must fail");
    assert!(
        matches!(error, ModelError::Config(message) if message.contains("call_1") && message.contains("call_2"))
    );

    let duplicate = request(vec![assistant_message(vec![
        tool_call_part("call_1", "get_date", json!({})),
        tool_call_part("call_1", "get_date", json!({})),
    ])]);
    let error =
        encode_request(&duplicate, &adapter, MODEL).expect_err("duplicate call id must fail");
    assert!(matches!(error, ModelError::Config(message) if message.contains("call_1")));
}

#[test]
fn canonical_native_canonical_round_trip_preserves_assistant_parts() {
    let adapter = reasoning_adapter();
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
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_1"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("sunny".to_owned()),
                metadata: None,
            },
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_2"),
            result: ToolResult {
                call_id: call_id("call_2"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("12:00".to_owned()),
                metadata: None,
            },
        }),
    ]);

    let encoded = encode_request(&req, &adapter, MODEL).expect("encode request");
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

    let parts = decode_assistant_message(assistant, &adapter).expect("decode assistant message");
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
