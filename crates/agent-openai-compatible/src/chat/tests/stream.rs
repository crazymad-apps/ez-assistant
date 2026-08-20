use super::*;

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
    let parts = decode_assistant_message(&message, &reasoning_adapter())
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
    let mut assembler = ChunkAssembler::new(reasoning_adapter());
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
    let mut assembler = ChunkAssembler::new(reasoning_adapter());
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
    let mut assembler = ChunkAssembler::new(reasoning_adapter());
    assert!(matches!(assembler.finalize(), Err(ModelError::Protocol(_))));

    // 没有 finish_reason 就结束：严格语义下属于协议违例。
    let mut assembler = ChunkAssembler::new(reasoning_adapter());
    feed(&mut assembler, vec![content_chunk("Hi")]).expect("chunk assembles");
    assert!(matches!(assembler.finalize(), Err(ModelError::Protocol(_))));
}

#[test]
fn stream_accepts_tool_call_turn_without_reasoning() {
    let mut assembler = ChunkAssembler::new(ChatProtocolAdapter::deepseek());
    feed(
        &mut assembler,
        vec![
            tool_chunk(json!([{
                "index": 0,
                "id": "call_1",
                "function": {"name": "shell", "arguments": "{}"}
            }])),
            finish_chunk("tool_calls"),
        ],
    )
    .expect("tool call chunks assemble before final response validation");

    let events = assembler
        .finalize()
        .expect("provider-returned tool call remains executable");
    let Some(ModelEvent::TurnFinished { message }) = events.last() else {
        panic!("expected TurnFinished");
    };
    assert_eq!(
        message.parts,
        vec![AssistantPart::ToolCall(ToolCall {
            id: call_id("call_1"),
            name: tool_name("shell"),
            arguments: json!({}),
        })]
    );
}

#[test]
fn stream_accepts_empty_null_and_text_only_reasoning_on_tool_call_turns() {
    let cases = [
        vec![reasoning_chunk("")],
        vec![chunk(json!({
            "id": "chatcmpl_1",
            "model": "deepseek-reasoner",
            "choices": [{"index": 0, "delta": {"reasoning_content": null}}],
        }))],
        vec![content_chunk("I will use a tool")],
    ];
    for prefix in cases {
        let mut chunks = prefix;
        chunks.push(tool_chunk(json!([{
            "index": 0,
            "id": "call_1",
            "function": {"name": "shell", "arguments": "{}"}
        }])));
        chunks.push(finish_chunk("tool_calls"));
        let events = assemble_with_adapter(ChatProtocolAdapter::deepseek(), chunks)
            .expect("provider-returned tool call remains executable");
        assert!(
            matches!(events.last(), Some(ModelEvent::TurnFinished { message }) if message
                .parts
                .iter()
                .any(|part| matches!(part, AssistantPart::ToolCall(call) if call.id == call_id("call_1"))))
        );
    }
}

#[test]
fn stream_rejects_choice_chunk_after_finish_but_accepts_usage() {
    let mut assembler = ChunkAssembler::new(reasoning_adapter());
    feed(&mut assembler, vec![finish_chunk("stop")]).expect("finish assembles");
    let result = assembler.push_chunk(&content_chunk("late"));
    assert!(matches!(result, Err(ModelError::Protocol(_))));

    // finish_reason 之后的空 choices usage chunk 是合法的流尾形态。
    let mut assembler = ChunkAssembler::new(reasoning_adapter());
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
