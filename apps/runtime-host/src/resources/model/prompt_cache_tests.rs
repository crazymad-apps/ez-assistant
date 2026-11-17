//! 缓存身份须进入最终请求，并在服务重建、会话追加与重发时保持稳定。
use super::*;
use agent_model::{ModelCallContext, ModelEvent, ModelRequest};
use agent_openai_compatible::{Transport, TransportFuture, TransportRequest, TransportResponse};
use futures_util::StreamExt;
use std::sync::Mutex;

#[derive(Default)]
struct CaptureTransport(Mutex<Vec<TransportRequest>>);

impl Transport for CaptureTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        self.0.lock().unwrap().push(request);
        Box::pin(async {
            Ok(TransportResponse {
                status: 200,
                headers: vec![],
                body: Box::pin(futures_util::stream::once(async {
                    Ok(include_bytes!(
                        "../../../../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse"
                    ).to_vec())
                })),
            })
        })
    }
}

#[tokio::test]
async fn responses_cache_key_uses_bound_owner_and_preserves_explicit_options() {
    let home = tempfile::tempdir().unwrap();
    let factory = HostModelServiceFactory::new(home.path());
    let capabilities = assistant_runtime::ResolvedModelCapabilities {
        image_input: false,
        reasoning: None,
        tool_calls: false,
        tool_image_projection: agent_model::ToolImageProjection::Unsupported,
        tool_choice: agent_model::ToolChoiceCapabilities::default(),
        streaming: true,
    };
    for (provider, owner, explicit, expected) in [
        ("openai", Some("main-owner"), None, Some("main-owner")),
        ("openai", Some("child-owner"), None, Some("child-owner")),
        (
            "openai",
            Some("main-owner"),
            Some("caller-key"),
            Some("caller-key"),
        ),
        ("openai", None, None, None),
        ("opencode", Some("main-owner"), None, None),
        ("local", Some("main-owner"), None, None),
    ] {
        let provider = agent_types::ProviderId::new(provider).unwrap();
        let transport = Arc::new(CaptureTransport::default());
        let mut input = ModelRequest {
            system: Default::default(),
            conversation: Default::default(),
            tools: vec![],
            tool_choice: agent_types::ToolChoice::Auto,
            generation: Default::default(),
            reasoning: None,
            provider_options: Default::default(),
        };
        if let Some(key) = explicit {
            input
                .provider_options
                .insert(
                    provider.as_str().to_owned(),
                    serde_json::json!({"prompt_cache_key": key}),
                )
                .unwrap();
        }
        for call in 0..3 {
            // 每次重建服务；最后一次追加输入，前两次模拟同一请求重发。
            if call == 2 {
                input
                    .conversation
                    .messages
                    .push(agent_types::ConversationMessage::User(
                        agent_types::UserMessage {
                            origin: Default::default(),
                            transcript_visibility: Default::default(),
                            id: agent_types::MessageId::new("next-user").unwrap(),
                            parts: vec![agent_types::UserPart::Text(agent_types::TextPart {
                                id: agent_types::PartId::new("next-text").unwrap(),
                                text: "next step".into(),
                            })],
                        },
                    ));
            }
            let service = factory
                .create_with_transport(
                    ModelServiceFactoryRequest {
                        conversation_routing_id: owner,
                        context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                        server_history_binding: None,
                        provider_state_binding: None,
                        input_token_count_supported: false,
                        compaction_threshold_tokens: std::num::NonZeroU64::new(1024).unwrap(),
                        external_configuration: None,
                        provider: &provider,
                        protocol: ModelProtocol::OpenAiResponses,
                        capabilities: &capabilities,
                        endpoint: "https://center.test/api/llm/providers/fixture/v1",
                        model: "fixture",
                        api_key: "test-key",
                        context_window_tokens: 8192,
                        max_input_tokens: None,
                        connect_timeout: std::time::Duration::from_secs(1),
                        request_timeout: std::time::Duration::from_secs(1),
                    },
                    Some(transport.clone()),
                )
                .unwrap()
                .model;
            let events = service
                .stream(input.clone(), ModelCallContext::default())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert!(
                matches!(events.last(), Some(ModelEvent::TurnFinished { .. })),
                "{events:?}"
            );
        }
        let requests = transport.0.lock().unwrap();
        assert_eq!(requests.len(), 3);
        for request in requests.iter() {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            match expected {
                Some(key) => assert_eq!(body["prompt_cache_key"], key),
                None => assert!(body.get("prompt_cache_key").is_none()),
            }
        }
        assert_eq!(requests[0].body, requests[1].body);
        assert_ne!(requests[1].body, requests[2].body);
    }
}
