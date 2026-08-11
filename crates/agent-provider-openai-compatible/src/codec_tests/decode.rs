use super::*;

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
fn decode_accepts_missing_empty_null_and_text_only_reasoning_on_tool_call_turns() {
    let cases = [
        (
            "missing",
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            }),
        ),
        (
            "empty",
            json!({
                "role": "assistant",
                "reasoning_content": "",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            }),
        ),
        (
            "null",
            json!({
                "role": "assistant",
                "reasoning_content": null,
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            }),
        ),
        (
            "text-only",
            json!({
                "role": "assistant",
                "reasoning_content": null,
                "content": "I will use a tool",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "shell", "arguments": "{}"}
                }]
            }),
        ),
    ];
    for (label, message) in cases {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": format!("chatcmpl_{label}"),
            "model": "deepseek-v4-flash",
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("parse response");

        let message = decode_response(&response, &Profile::deepseek())
            .expect("provider-returned tool call remains executable");
        assert!(
            !message
                .parts
                .iter()
                .any(|part| matches!(part, AssistantPart::Reasoning(_))),
            "case {label} must not fabricate canonical reasoning"
        );
        assert!(
            message.parts.iter().any(
                |part| matches!(part, AssistantPart::ToolCall(call) if call.id == call_id("call_1"))
            ),
            "case {label} must preserve the tool call"
        );
    }
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
        ModelError::RateLimited {
            message: "slow down".to_owned(),
            retry_after_ms: None,
        }
    );

    let body: ChatErrorBody =
        serde_json::from_str(include_str!("../../fixtures/errors/context_overflow.json"))
            .expect("parse context overflow fixture");
    assert_eq!(
        decode_error_body(&body),
        ModelError::ContextOverflow {
            message: "request is too large".to_owned(),
        }
    );

    let body: ChatErrorBody = serde_json::from_value(json!({
        "error": {
            "message": "request is too large",
            "type": "context_length_exceeded"
        },
    }))
    .expect("parse error body");
    assert_eq!(
        decode_error_body(&body),
        ModelError::ContextOverflow {
            message: "request is too large".to_owned(),
        }
    );

    // 未经 fixture 确认的相似 code 不得通过文本或模糊匹配升级为 Overflow。
    let body: ChatErrorBody = serde_json::from_value(json!({
        "error": {
            "message": "context_length_exceeded appears only in the message",
            "type": "invalid_request_error",
            "code": "context_window_too_large"
        },
    }))
    .expect("parse error body");
    assert_eq!(
        decode_error_body(&body),
        ModelError::Provider {
            message: "context_length_exceeded appears only in the message".to_owned(),
            status: None,
        }
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
