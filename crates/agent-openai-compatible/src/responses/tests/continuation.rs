use super::*;

fn service(transport: Arc<RecordedTransport>, binding: Option<&str>) -> OpenAiResponsesService {
    let service = OpenAiResponsesService::with_transport(
        "https://api.openai.test/v1",
        BearerCredential::new("secret"),
        "fixture-model",
        128000,
        adapter(),
        transport,
    )
    .unwrap();
    match binding {
        Some(binding) => service.with_server_history(binding).unwrap(),
        None => service,
    }
}

async fn turn(service: &OpenAiResponsesService, input: ModelRequest) -> AssistantMessage {
    let stream = service
        .stream(input, ModelCallContext::default())
        .await
        .unwrap();
    EventCollector::collect_validated(stream)
        .await
        .assert_finished()
        .clone()
}

fn body(transport: &RecordedTransport) -> Value {
    serde_json::from_slice(&transport.take_requests().pop().unwrap().body).unwrap()
}

#[tokio::test]
async fn complete_text_roundtrip_restores_chain_in_a_new_service_and_resends_current_parameters() {
    let first = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, TEXT_SSE,
    ))]));
    let initial = request(vec![user("u1", "hello")]);
    let message = turn(
        &service(first.clone(), Some("owner:connection")),
        initial.clone(),
    )
    .await;
    let sent = body(&first);
    assert_eq!(sent["store"], true);
    assert!(sent.get("previous_response_id").is_none());
    let stored: AssistantMessage =
        serde_json::from_slice(&serde_json::to_vec(&message).unwrap()).unwrap();
    assert!(!format!("{stored:?}").contains("owner:connection"));
    let second = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, TEXT_SSE,
    ))]));
    let input = request(vec![
        initial.conversation.messages[0].clone(),
        ConversationMessage::Assistant(stored),
        user("u2", "next"),
    ]);
    turn(
        &service(second.clone(), Some("owner:connection")),
        input.clone(),
    )
    .await;
    let sent = body(&second);
    assert_eq!(sent["previous_response_id"], message.id.as_str());
    assert_eq!(sent["input"].as_array().unwrap().len(), 1);
    assert_eq!(sent["input"][0]["content"][0]["text"], "next");
    for binding in [
        None,
        Some("owner:new-connection"),
        Some("other-owner:connection"),
    ] {
        let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            200, TEXT_SSE,
        ))]));
        turn(&service(transport.clone(), binding), input.clone()).await;
        let sent = body(&transport);
        assert!(sent.get("previous_response_id").is_none());
        assert_eq!(sent["input"].as_array().unwrap().len(), 3);
        assert!(!sent.to_string().contains("responses.continuation"));
    }
    for changed in ["history", "instructions", "generation"] {
        let mut modified = input.clone();
        match changed {
            "history" => modified.conversation.messages[0] = user("u1", "rewritten"),
            "instructions" => {
                modified.system = SystemPromptSnapshot::new(vec!["new instruction".into()])
            }
            _ => modified.generation.max_output_tokens = Some(128),
        }
        let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            200, TEXT_SSE,
        ))]));
        turn(
            &service(transport.clone(), Some("owner:connection")),
            modified,
        )
        .await;
        assert!(body(&transport).get("previous_response_id").is_none());
    }
}

#[tokio::test]
async fn committed_function_batch_sends_only_outputs_and_pending_batch_cannot_resume() {
    let first = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        FUNCTION_SSE,
    ))]));
    let initial = request(vec![user("u1", "tools")]);
    let message = turn(&service(first, Some("owner:connection")), initial.clone()).await;
    let mut input = initial;
    input
        .conversation
        .messages
        .push(ConversationMessage::Assistant(message.clone()));
    let rejected = service(
        Arc::new(RecordedTransport::new([])),
        Some("owner:connection"),
    );
    assert!(matches!(
        rejected
            .stream(input.clone(), ModelCallContext::default())
            .await,
        Err(ModelError::Config(_))
    ));
    let mut calls = 0;
    for part in &message.parts {
        if let AssistantPart::ToolCall(call) = part {
            calls += 1;
            input
                .conversation
                .messages
                .push(ConversationMessage::Tool(ToolMessage {
                    id: message_id(&format!("output-{calls}")),
                    result: ToolResult {
                        call_id: call.id.clone(),
                        status: ToolResultStatus::Success,
                        content: ToolResultContent::text("done"),
                        metadata: Default::default(),
                    },
                }));
        }
    }
    let next = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, TEXT_SSE,
    ))]));
    turn(&service(next.clone(), Some("owner:connection")), input).await;
    let sent = body(&next);
    assert_eq!(sent["previous_response_id"], message.id.as_str());
    assert_eq!(sent["input"].as_array().unwrap().len(), calls);
    assert!(
        sent["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] == "function_call_output")
    );
}

#[tokio::test]
async fn incomplete_or_broken_stream_does_not_publish_a_continuation() {
    let incomplete = TEXT_SSE.replace("response.completed", "response.incomplete");
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, incomplete,
    ))]));
    let message = turn(
        &service(transport, Some("owner:connection")),
        request(vec![user("u1", "hello")]),
    )
    .await;
    assert!(!message.parts.iter().any(|part|matches!(part,AssistantPart::ProviderState(state) if state.state_type()==super::super::continuation::STATE_TYPE)));
    let broken = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        TEXT_SSE
            .split("data: {\"type\":\"response.completed\"")
            .next()
            .unwrap(),
    ))]));
    let model = service(broken, Some("owner:connection"));
    let stream = model
        .stream(
            request(vec![user("u1", "hello")]),
            ModelCallContext::default(),
        )
        .await
        .unwrap();
    let events = EventCollector::collect_validated(stream).await;
    assert!(
        !events
            .events()
            .iter()
            .any(|event| matches!(event, ModelEvent::TurnFinished { .. }))
    );
}

#[tokio::test]
async fn only_an_exact_establishment_rejection_is_history_unavailable_and_rebuild_keeps_the_binding()
 {
    let initial = request(vec![user("u1", "hello")]);
    let first = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, TEXT_SSE,
    ))]));
    let message = turn(&service(first, Some("owner:connection")), initial.clone()).await;
    let input = request(vec![
        initial.conversation.messages[0].clone(),
        ConversationMessage::Assistant(message),
        user("u2", "next"),
    ]);
    for (status, code, kind, param, expected) in [
        (
            400,
            "previous_response_not_found",
            "invalid_request_error",
            "previous_response_id",
            true,
        ),
        (
            404,
            "previous_response_not_found",
            "invalid_request_error",
            "previous_response_id",
            true,
        ),
        (
            401,
            "previous_response_not_found",
            "invalid_request_error",
            "previous_response_id",
            false,
        ),
        (
            500,
            "previous_response_not_found",
            "invalid_request_error",
            "previous_response_id",
            false,
        ),
        (
            400,
            "unknown",
            "invalid_request_error",
            "previous_response_id",
            false,
        ),
        (
            400,
            "previous_response_not_found",
            "invalid_request_error",
            "model",
            false,
        ),
        (
            404,
            "previous_response_not_found",
            "other",
            "previous_response_id",
            false,
        ),
    ] {
        let error = serde_json::json!({"error":{"message":"private upstream sample","code":code,"type":kind,"param":param}}).to_string();
        let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            status, error,
        ))]));
        let result = service(transport, Some("owner:connection"))
            .stream(input.clone(), ModelCallContext::default())
            .await;
        assert_eq!(
            matches!(result, Err(ModelError::HistoryUnavailable)),
            expected
        );
    }
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, TEXT_SSE,
    ))]));
    let stream = service(transport.clone(), Some("owner:connection"))
        .stream(
            input,
            ModelCallContext {
                rebuild_server_history: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    EventCollector::collect_validated(stream)
        .await
        .assert_finished();
    let sent = body(&transport);
    assert_eq!(sent["store"], true);
    assert!(sent.get("previous_response_id").is_none());
    assert_eq!(sent["input"].as_array().unwrap().len(), 3);
    assert!(!sent.to_string().contains("responses.continuation"));
}

#[tokio::test]
async fn raw_and_summary_history_resume_after_serialization_without_reclassification() {
    for summary in [false, true] {
        let content = json!({"type": if summary { "summary_text" } else { "reasoning_text" }, "text":"visible reasoning"});
        let mut item =
            json!({"id":"reasoning-source", "type":"reasoning", "summary":[], "content":[]});
        item[if summary { "summary" } else { "content" }] = json!([content]);
        let frames = [
            json!({"type":"response.created","response":{"id":"source-response","model":"fixture-model"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"id":"reasoning-source","type":"reasoning","summary":[],"content":[]}}),
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
            json!({"type":"response.completed","response":{"id":"source-response","model":"fixture-model"}}),
        ];
        let sse = frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect::<String>();
        let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            200, sse,
        ))]));
        let initial = request(vec![user("source-user", "question")]);
        let message = turn(&service(transport, Some("source-owner")), initial.clone()).await;
        let bytes = serde_json::to_vec(&message).unwrap();
        let restored: AssistantMessage = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
        let AssistantPart::Reasoning(part) = &restored.parts[0] else {
            panic!("reasoning")
        };
        assert_eq!(
            part.source == agent_types::ReasoningSource::Summary,
            summary
        );
        let second = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            200, TEXT_SSE,
        ))]));
        turn(
            &service(second.clone(), Some("source-owner")),
            request(vec![
                initial.conversation.messages[0].clone(),
                ConversationMessage::Assistant(restored),
                user("next-user", "continue"),
            ]),
        )
        .await;
        let sent = body(&second);
        assert_eq!(sent["previous_response_id"], "source-response");
        assert_eq!(sent["input"].as_array().unwrap().len(), 1);
    }
}
