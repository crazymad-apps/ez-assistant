use super::*;
use std::time::Duration;

use futures_util::StreamExt;

use crate::{Transport, TransportError, TransportFuture, TransportRequest};

fn compact_window(id: &str) -> Value {
    json!({"object":"response.compaction", "id":id, "created_at":0,
        "output":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Keep the task constraints"}]}, {"type":"compaction","encrypted_content":"synthetic-opaque-state"}],
        "usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}})
}

fn native_service(transport: Arc<dyn Transport>, scope: &str) -> OpenAiResponsesService {
    OpenAiResponsesService::with_transport(
        "https://provider.invalid/v1",
        BearerCredential::new("synthetic-secret"),
        "fixture-model",
        128_000,
        adapter(),
        transport,
    )
    .unwrap()
    .with_server_compaction(scope, std::num::NonZeroU64::new(100_000).unwrap())
    .unwrap()
    .with_server_history(scope)
    .unwrap()
}

/// 用可控时钟复现慢压缩，响应建立与正文空闲沿用 Transport 的独立超时语义。
struct DelayedTransport {
    inner: RecordedTransport,
    headers_delay: Duration,
    body_delay: Duration,
    request_timeout: Duration,
}

impl Transport for DelayedTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        Box::pin(async move {
            tokio::time::timeout(self.request_timeout, tokio::time::sleep(self.headers_delay))
                .await
                .map_err(|_| TransportError::Timeout)?;
            let mut response = self.inner.execute(request).await?;
            let body_delay = self.body_delay;
            let request_timeout = self.request_timeout;
            response.body = Box::pin(response.body.then(move |chunk| async move {
                tokio::time::timeout(request_timeout, tokio::time::sleep(body_delay))
                    .await
                    .map_err(|_| TransportError::Timeout)?;
                chunk
            }));
            Ok(response)
        })
    }
}

fn delayed_transport(
    headers_seconds: u64,
    body_seconds: u64,
    timeout_seconds: u64,
) -> Arc<DelayedTransport> {
    Arc::new(DelayedTransport {
        inner: RecordedTransport::new([Ok(RecordedResponse::new(
            200,
            compact_window("slow-window").to_string(),
        ))]),
        headers_delay: Duration::from_secs(headers_seconds),
        body_delay: Duration::from_secs(body_seconds),
        request_timeout: Duration::from_secs(timeout_seconds),
    })
}

#[tokio::test(start_paused = true)]
async fn compaction_waits_beyond_two_minutes_for_headers_and_complete_body() {
    let service = native_service(delayed_transport(150, 150, 300), "scope");
    let compact = service
        .compact(
            request(vec![user("task", "task")]),
            ModelCallContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(compact.id.as_str(), "slow-window");
}

#[tokio::test(start_paused = true)]
async fn compaction_preserves_transport_timeouts_at_headers_and_body() {
    for (headers, body) in [(150, 0), (0, 150)] {
        let service = native_service(delayed_transport(headers, body, 75), "scope");
        let started = tokio::time::Instant::now();
        let result = service
            .compact(request(vec![]), ModelCallContext::default())
            .await;
        assert!(matches!(
            result,
            Err(ModelError::Transport {
                kind: agent_model::ModelTransportErrorKind::Timeout,
                ..
            })
        ));
        assert_eq!(started.elapsed(), Duration::from_secs(75));
    }
}

#[tokio::test(start_paused = true)]
async fn compaction_can_be_cancelled_while_waiting_for_headers_or_body() {
    for (headers, body) in [(150, 0), (0, 150)] {
        let service = native_service(delayed_transport(headers, body, 300), "scope");
        let context = ModelCallContext::default();
        let cancellation = context.cancellation.clone();
        let (result, ()) = tokio::join!(service.compact(request(vec![]), context), async move {
            tokio::time::sleep(Duration::from_secs(125)).await;
            cancellation.cancel();
        });
        assert!(matches!(result, Err(ModelError::Cancelled)));
    }
}

#[tokio::test(start_paused = true)]
async fn input_count_retains_its_independent_three_second_budget() {
    for (headers, body) in [(4, 0), (0, 4)] {
        let service = native_service(delayed_transport(headers, body, 300), "scope")
            .with_input_token_count(true);
        let started = tokio::time::Instant::now();
        let result = service
            .count_input_tokens(&request(vec![]), &mut ModelCallContext::default())
            .await;
        assert!(matches!(
            result,
            Err(ModelError::Transport {
                kind: agent_model::ModelTransportErrorKind::Timeout,
                ..
            })
        ));
        assert_eq!(started.elapsed(), Duration::from_secs(3));
    }
}

async fn response_turn(service: &OpenAiResponsesService, input: ModelRequest) -> AssistantMessage {
    EventCollector::collect_validated(
        service
            .stream(input, ModelCallContext::default())
            .await
            .unwrap(),
    )
    .await
    .assert_finished()
    .clone()
}

fn sent(transport: &RecordedTransport) -> Vec<Value> {
    transport
        .take_requests()
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

#[tokio::test]
async fn compact_preserves_complete_window_and_starts_then_resumes_a_new_chain() {
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(
            200,
            compact_window("compact-one").to_string(),
        )),
        Ok(RecordedResponse::new(200, TEXT_SSE)),
        Ok(RecordedResponse::new(200, TEXT_SSE)),
        Ok(RecordedResponse::new(
            200,
            compact_window("compact-two").to_string(),
        )),
        Ok(RecordedResponse::new(200, TEXT_SSE)),
    ]));
    let service = native_service(transport.clone(), "session:stable-configuration");
    let original = request(vec![user("task", "original tool history")]);
    let compact = service
        .compact(original.clone(), ModelCallContext::default())
        .await
        .unwrap();
    assert_eq!(
        compact.purpose,
        agent_types::AssistantMessagePurpose::ContextCompaction
    );
    assert!(!ConversationMessage::Assistant(compact.clone()).is_transcript_visible());
    let AssistantPart::ProviderState(state) = &compact.parts[0] else {
        panic!("native state");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(state.payload()).unwrap(),
        compact_window("compact-one")
    );
    let mut after = original;
    after
        .conversation
        .messages
        .push(ConversationMessage::Assistant(compact));
    let normal = response_turn(&service, after.clone()).await;
    after
        .conversation
        .messages
        .push(ConversationMessage::Assistant(normal.clone()));
    after
        .conversation
        .messages
        .push(user("next", "next question"));
    response_turn(&service, after.clone()).await;
    let compact = service
        .compact(after.clone(), ModelCallContext::default())
        .await
        .unwrap();
    after
        .conversation
        .messages
        .push(ConversationMessage::Assistant(compact));
    response_turn(&service, after).await;
    let requests = sent(&transport);
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0].as_object().unwrap().len(), 2);
    assert_eq!(
        requests[1]["input"],
        compact_window("compact-one")["output"]
    );
    assert!(requests[1].get("previous_response_id").is_none());
    assert_eq!(requests[2]["previous_response_id"], normal.id.as_str());
    assert_eq!(requests[2]["input"].as_array().unwrap().len(), 1);
    assert!(requests[3].get("previous_response_id").is_none());
    assert!(requests[4].get("previous_response_id").is_none());
    assert_eq!(
        requests[4]["input"],
        compact_window("compact-two")["output"]
    );
}

#[tokio::test]
async fn malformed_partial_and_unpaired_compact_windows_are_rejected_without_generation() {
    for mutation in ["missing", "unfinished", "unknown", "pending", "orphan"] {
        let mut window = compact_window("bad-window");
        match mutation {
            "missing" => window["output"] = json!([]),
            "unfinished" => window["status"] = json!("incomplete"),
            "unknown" => window["output"][1]["type"] = json!("unknown_item"),
            "pending" => window["output"].as_array_mut().unwrap().insert(0, json!({"type":"function_call","call_id":"call-one","name":"lookup","arguments":"{}"})),
            _ => window["output"].as_array_mut().unwrap().push(json!({"type":"function_call_output","call_id":"missing-call","output":"result"})),
        }
        let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            200,
            window.to_string(),
        ))]));
        let result = native_service(transport.clone(), "scope")
            .compact(
                request(vec![user("task", "task")]),
                ModelCallContext::default(),
            )
            .await;
        assert!(matches!(result, Err(ModelError::Protocol(_))), "{mutation}");
        let requests = transport.take_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.ends_with("/responses/compact"));
    }
}

#[tokio::test]
async fn input_count_is_independent_and_never_creates_a_response_chain() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        json!({"input_tokens":456}).to_string(),
    ))]));
    let service = native_service(transport.clone(), "scope").with_input_token_count(true);
    let mut context = ModelCallContext::default();
    assert_eq!(
        service
            .count_input_tokens(&request(vec![user("task", "task")]), &mut context)
            .await
            .unwrap(),
        Some(456)
    );
    let request = transport.take_requests().pop().unwrap();
    assert!(request.url.ends_with("/responses/input_tokens"));
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert!(body.get("previous_response_id").is_none());
    assert!(body.get("store").is_none());
    assert!(body.get("stream").is_none());
    let disabled = Arc::new(RecordedTransport::new([]));
    assert_eq!(
        native_service(disabled.clone(), "scope")
            .count_input_tokens(&super::request(vec![]), &mut context)
            .await
            .unwrap(),
        None
    );
    assert!(disabled.take_requests().is_empty());
}

#[tokio::test]
async fn interrupted_cancelled_oversized_and_overflow_compaction_never_publish_state() {
    let mut oversized = compact_window("large-window");
    oversized["output"][1]["encrypted_content"] = json!("x".repeat(32 * 1024 * 1024));
    let responses = [
        RecordedResponse::chunked(200, vec![BodyStep::Chunk(compact_window("partial-window").to_string().into_bytes()), BodyStep::Fail("synthetic disconnect".into())]),
        RecordedResponse::new(200, oversized.to_string()),
        RecordedResponse::new(400, json!({"error":{"message":"too large","code":"context_length_exceeded","type":"invalid_request_error"}}).to_string()),
    ];
    for (index, response) in responses.into_iter().enumerate() {
        let transport = Arc::new(RecordedTransport::new([Ok(response)]));
        let result = native_service(transport.clone(), "scope")
            .compact(
                request(vec![user("task", "task")]),
                ModelCallContext::default(),
            )
            .await;
        assert!(result.is_err());
        if index == 1 {
            assert!(matches!(
                &result,
                Err(ModelError::Protocol(message)) if message.contains("32 MiB transport budget")
            ));
        }
        if index == 2 {
            assert!(matches!(result, Err(ModelError::ContextOverflow { .. })));
        }
        assert_eq!(transport.take_requests().len(), 1);
    }
    let transport = Arc::new(RecordedTransport::new([]));
    let context = ModelCallContext::default();
    context.cancellation.cancel();
    assert!(matches!(
        native_service(transport.clone(), "scope")
            .compact(request(vec![]), context)
            .await,
        Err(ModelError::Cancelled)
    ));
    assert!(transport.take_requests().is_empty());
}

#[tokio::test]
async fn complete_window_with_large_retained_image_roundtrips_and_replays_without_item_limits() {
    let mut window = compact_window("large-image-window");
    let image = format!(
        "data:image/png;base64,{}",
        "AAAA".repeat(agent_types::MAX_PROVIDER_STATE_TURN_BYTES / 4 + 1)
    );
    window["output"][0]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"input_image","image_url":image}));
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(200, window.to_string())),
        Ok(RecordedResponse::new(
            200,
            json!({"input_tokens":100}).to_string(),
        )),
        Ok(RecordedResponse::new(200, TEXT_SSE)),
        Ok(RecordedResponse::new(200, window.to_string())),
    ]));
    let service = native_service(transport.clone(), "scope").with_input_token_count(true);
    let compact = service
        .compact(
            request(vec![user("old", "old history")]),
            ModelCallContext::default(),
        )
        .await
        .unwrap();
    let stored = serde_json::to_vec(&compact).unwrap();
    let restored: AssistantMessage = serde_json::from_slice(&stored).unwrap();
    assert_eq!(compact, restored);
    let replay = request(vec![
        ConversationMessage::Assistant(restored),
        user("continue", "continue after large image"),
    ]);
    assert_eq!(
        service
            .count_input_tokens(&replay, &mut ModelCallContext::default())
            .await
            .unwrap(),
        Some(100)
    );
    response_turn(&service, replay.clone()).await;
    service
        .compact(replay, ModelCallContext::default())
        .await
        .unwrap();
    for encoded in transport.take_requests().into_iter().skip(1) {
        let body: Value = serde_json::from_slice(&encoded.body).unwrap();
        assert_eq!(body["input"][0], window["output"][0]);
        assert_eq!(body["input"][1], window["output"][1]);
        assert_eq!(
            body["input"][2]["content"][0]["text"],
            "continue after large image"
        );
    }
}

#[tokio::test]
async fn native_window_survives_model_endpoint_and_continuation_binding_changes() {
    let mut window = compact_window("image-window");
    window["output"][0]["content"].as_array_mut().unwrap().push(
        json!({"type":"input_image","image_url":"data:image/png;base64,AA==","detail":"high"}),
    );
    let transport = Arc::new(RecordedTransport::new([
        Ok(RecordedResponse::new(200, window.to_string())),
        Ok(RecordedResponse::new(200, TEXT_SSE)),
    ]));
    let service = native_service(transport.clone(), "original-owner");
    let mut input = request(vec![user("task", "original facts")]);
    let message = service
        .compact(input.clone(), ModelCallContext::default())
        .await
        .unwrap();
    input
        .conversation
        .messages
        .push(ConversationMessage::Assistant(message));
    let normal = response_turn(&service, input.clone()).await;
    input
        .conversation
        .messages
        .push(ConversationMessage::Assistant(normal));
    input.conversation.messages.push(user("next", "continue"));
    assert_eq!(sent(&transport)[1]["input"], window["output"]);

    for (endpoint, model, binding) in [
        ("https://provider.invalid/v1", "new-model", "original-owner"),
        (
            "https://new-entry.invalid/v1",
            "fixture-model",
            "original-owner",
        ),
        ("https://provider.invalid/v1", "fixture-model", "new-owner"),
        ("https://new-entry.invalid/v1", "new-model", "new-owner"),
    ] {
        let changed = Arc::new(RecordedTransport::new([
            Ok(RecordedResponse::new(200, r#"{"input_tokens":42}"#)),
            Ok(RecordedResponse::new(200, TEXT_SSE)),
            Ok(RecordedResponse::new(
                200,
                compact_window("next-window").to_string(),
            )),
        ]));
        let service = OpenAiResponsesService::with_transport(
            endpoint,
            BearerCredential::new("synthetic-secret"),
            model,
            128_000,
            adapter(),
            changed.clone(),
        )
        .unwrap()
        .with_server_compaction(binding, std::num::NonZeroU64::new(100_000).unwrap())
        .unwrap()
        .with_server_history(binding)
        .unwrap()
        .with_input_token_count(true);
        assert_eq!(
            service
                .count_input_tokens(&input, &mut ModelCallContext::default())
                .await
                .unwrap(),
            Some(42)
        );
        response_turn(&service, input.clone()).await;
        service
            .compact(input.clone(), ModelCallContext::default())
            .await
            .unwrap();
        let requests = sent(&changed);
        assert_eq!(requests.len(), 3);
        for body in requests {
            let items = body["input"].as_array().unwrap();
            assert_eq!(
                &items[..2],
                window["output"].as_array().unwrap().as_slice(),
                "the saved window must survive changes to {endpoint}, {model}, {binding}"
            );
            assert_eq!(items.last().unwrap()["content"][0]["text"], "continue");
            assert!(!body.to_string().contains("original facts"));
            assert!(
                body.get("previous_response_id").is_none(),
                "a changed route must rebuild from the window instead of reusing the old response ID"
            );
            assert_eq!(body["model"], model);
        }
    }
}

#[tokio::test]
async fn changed_binding_rejects_invalid_native_window_instead_of_replaying_old_raw_history() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        compact_window("saved-window").to_string(),
    ))]));
    let original = request(vec![user("task", "raw history must not be resent")]);
    let compact = native_service(transport, "old-binding")
        .compact(original.clone(), ModelCallContext::default())
        .await
        .unwrap();
    for (field, value) in [
        ("format_version", json!(2)),
        ("media_type", json!("text/plain")),
        ("provider", json!("different-provider")),
        ("protocol", json!("different.protocol")),
    ] {
        let mut encoded = serde_json::to_value(&compact).unwrap();
        encoded["parts"][0]["data"][field] = value;
        let mut input = original.clone();
        input
            .conversation
            .messages
            .push(ConversationMessage::Assistant(
                serde_json::from_value(encoded).unwrap(),
            ));
        let target = Arc::new(RecordedTransport::new([]));
        let result = native_service(target.clone(), "new-binding")
            .stream(input, ModelCallContext::default())
            .await;
        assert!(
            matches!(result, Err(ModelError::Protocol(_))),
            "invalid {field} must fail explicitly"
        );
        assert!(target.take_requests().is_empty());
    }
}

#[tokio::test]
async fn native_mode_replays_agent_summary_as_system_message_without_handoff_compaction() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, TEXT_SSE,
    ))]));
    let service = native_service(transport.clone(), "existing-session");
    let summary = ConversationMessage::ContextSummary(ContextSummaryMessage {
        retained_user_message_id: None,
        id: message_id("old-agent-summary"),
        text: "Preserve the original task constraints".into(),
        model: None,
        usage: None,
        compacted_usage: None,
        usage_adjustment: None,
        programmatic_context: Some("cwd=/workspace".into()),
    });
    response_turn(
        &service,
        request(vec![
            user("ancient", "Already summarized history"),
            summary,
            user("next", "continue"),
        ]),
    )
    .await;
    let requests = transport.take_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.ends_with("/responses"));
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 2);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "system");
    let text = input[0]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Preserve the original task constraints"));
    assert!(text.contains("cwd=/workspace"));
    assert_eq!(input[1]["role"], "user");
    assert_eq!(input[1]["content"][0]["text"], "continue");
    assert!(body.get("previous_response_id").is_none());
}
