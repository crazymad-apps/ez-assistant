use super::*;
use crate::{OpenAiResponsesService, OpenCodeRequestIdentity, ResponsesProtocolAdapter};

#[test]
fn request_identity_rejects_unbounded_or_header_unsafe_values() {
    for invalid in ["".to_owned(), "a\r\nb".into(), "空".into(), "a".repeat(129)] {
        assert!(OpenCodeRequestIdentity::new(&invalid, "ez-assistant/test").is_err());
    }
    assert!(OpenCodeRequestIdentity::new("owner", "bad\nagent").is_err());
}

#[tokio::test]
async fn missing_identity_fails_before_transport_and_other_dialects_do_not_send_it() {
    for responses in [false, true] {
        let transport = Arc::new(RecordedTransport::new([]));
        let service: Arc<dyn ModelService> = if responses {
            Arc::new(
                OpenAiResponsesService::with_transport(
                    BASE_URL,
                    BearerCredential::new(TOKEN),
                    "fixture",
                    8192,
                    ResponsesProtocolAdapter::opencode(),
                    transport.clone(),
                )
                .unwrap(),
            )
        } else {
            Arc::new(
                OpenAiChatCompletionsService::with_transport(
                    BASE_URL,
                    BearerCredential::new(TOKEN),
                    "fixture",
                    8192,
                    ChatProtocolAdapter::opencode(),
                    transport.clone(),
                )
                .unwrap(),
            )
        };
        assert!(matches!(
            service
                .stream(simple_request(), ModelCallContext::default())
                .await,
            Err(ModelError::Config(_))
        ));
        assert!(transport.take_requests().is_empty());
    }
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        OK_SSE_BODY.as_bytes().to_vec(),
    ))]));
    let service = service_with(&transport).with_opencode_identity(
        OpenCodeRequestIdentity::new("owner", "ez-assistant/test").unwrap(),
    );
    let events = service
        .stream(simple_request(), ModelCallContext::default())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        events.last(),
        Some(agent_model::ModelEvent::TurnFinished { .. })
    ));
    assert!(
        transport.take_requests()[0]
            .headers
            .iter()
            .all(|(name, _)| name != "x-opencode-session" && name != "user-agent")
    );
}

#[tokio::test]
async fn retry_reuses_the_same_bound_opencode_identity_and_body() {
    use agent_model::{ModelRetryPolicy, ModelRetryReason, RetryingModelService};
    for responses in [false, true] {
        let body = if responses {
            include_str!("../../../fixtures/responses/openai/text.sse")
        } else {
            OK_SSE_BODY
        };
        let transport = Arc::new(RecordedTransport::new([
            Ok(RecordedResponse::new(503, b"unavailable".to_vec())),
            Ok(RecordedResponse::new(200, body.as_bytes().to_vec())),
        ]));
        let identity = OpenCodeRequestIdentity::new("stable-route", "ez-assistant/test").unwrap();
        let model: Arc<dyn ModelService> = if responses {
            Arc::new(
                OpenAiResponsesService::with_transport(
                    BASE_URL,
                    BearerCredential::new(TOKEN),
                    "fixture",
                    8192,
                    ResponsesProtocolAdapter::opencode(),
                    transport.clone(),
                )
                .unwrap()
                .with_opencode_identity(identity),
            )
        } else {
            Arc::new(
                OpenAiChatCompletionsService::with_transport(
                    BASE_URL,
                    BearerCredential::new(TOKEN),
                    "fixture",
                    8192,
                    ChatProtocolAdapter::opencode(),
                    transport.clone(),
                )
                .unwrap()
                .with_opencode_identity(identity),
            )
        };
        let service = RetryingModelService::new(
            model,
            ModelRetryPolicy::new(
                [ModelRetryReason::Unavailable].into_iter().collect(),
                vec![Duration::ZERO],
                Duration::ZERO,
            ),
        );
        let events = service
            .stream(simple_request(), ModelCallContext::default())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(
            events.last(),
            Some(agent_model::ModelEvent::TurnFinished { .. })
        ));
        let requests = transport.take_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].headers, requests[1].headers);
        assert_eq!(requests[0].body, requests[1].body);
        assert!(
            requests[0]
                .headers
                .contains(&("x-opencode-session".into(), "stable-route".into()))
        );
    }
}
