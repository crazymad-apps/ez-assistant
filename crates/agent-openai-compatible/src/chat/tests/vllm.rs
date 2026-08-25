use super::*;

#[test]
fn adapter_declares_current_and_legacy_vllm_reasoning_fields() {
    let adapter = ChatProtocolAdapter::vllm();
    assert_eq!(adapter.provider, provider_id("vllm"));
    assert_eq!(adapter.protocol, protocol_id("openai.chat_completions"));
    assert_eq!(
        adapter.reasoning_response_fields,
        vec!["reasoning", "reasoning_content"]
    );
    assert_eq!(adapter.reasoning_replay_field.as_deref(), Some("reasoning"));
    assert!(adapter.supports_reasoning());
    assert_eq!(
        adapter.reasoning_replay_policy(),
        crate::ReasoningReplayPolicy::Drop
    );
}

#[test]
fn decode_prefers_current_vllm_reasoning_and_falls_back_to_legacy_field() {
    for (label, message, expected) in [
        (
            "current",
            json!({
                "role": "assistant",
                "reasoning": "current thought",
                "content": "answer"
            }),
            "current thought",
        ),
        (
            "legacy",
            json!({
                "role": "assistant",
                "reasoning_content": "legacy thought",
                "content": "answer"
            }),
            "legacy thought",
        ),
        (
            "current-wins",
            json!({
                "role": "assistant",
                "reasoning": "current thought",
                "reasoning_content": "legacy duplicate",
                "content": "answer"
            }),
            "current thought",
        ),
    ] {
        let response: ChatResponse = serde_json::from_value(json!({
            "id": format!("chatcmpl_{label}"),
            "model": "qwen3.8-27b-4bit",
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": "stop"
            }]
        }))
        .expect("parse vLLM response");

        let decoded =
            decode_response(&response, &ChatProtocolAdapter::vllm()).expect("decode vLLM response");
        assert!(matches!(
            decoded.parts.as_slice(),
            [AssistantPart::Reasoning(reasoning), AssistantPart::Text(text)]
                if reasoning.text == expected && text.text == "answer"
        ));
    }
}

#[test]
fn stream_maps_vllm_reasoning_delta_to_canonical_events() {
    let events = assemble_with_adapter(
        ChatProtocolAdapter::vllm(),
        vec![
            chunk(json!({
                "id": "chatcmpl_vllm",
                "model": "qwen3.8-27b-4bit",
                "choices": [{"index": 0, "delta": {"reasoning": "think"}}]
            })),
            chunk(json!({
                "id": "chatcmpl_vllm",
                "model": "qwen3.8-27b-4bit",
                "choices": [{"index": 0, "delta": {"content": "answer"}}]
            })),
            chunk(json!({
                "id": "chatcmpl_vllm",
                "model": "qwen3.8-27b-4bit",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            })),
        ],
    )
    .expect("assemble vLLM stream");

    assert!(events.iter().any(|event| matches!(
        event,
        ModelEvent::ReasoningDelta { delta, .. } if delta == "think"
    )));
    let Some(ModelEvent::TurnFinished { message }) = events.last() else {
        panic!("vLLM stream must finish with a canonical message");
    };
    assert!(matches!(
        message.parts.as_slice(),
        [AssistantPart::Reasoning(reasoning), AssistantPart::Text(text)]
            if reasoning.text == "think" && text.text == "answer"
    ));
}

#[test]
fn replay_uses_current_vllm_reasoning_field() {
    let adapter = ChatProtocolAdapter::vllm()
        .with_reasoning_replay(crate::ReasoningReplayPolicy::PreserveAll);
    let encoded = encode_request(
        &request(vec![assistant_message(vec![reasoning_part(
            "historic thought",
        )])]),
        &adapter,
        "qwen3.8-27b-4bit",
    )
    .expect("encode vLLM history");
    let value = serde_json::to_value(encoded).expect("serialize vLLM request");

    assert_eq!(value["messages"][0]["reasoning"], json!("historic thought"));
    assert!(value["messages"][0].get("reasoning_content").is_none());
}
