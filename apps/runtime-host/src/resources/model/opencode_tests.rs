//! 正式 Host 工厂生成的双协议请求：企业 URL 不能抹去方言身份。
use super::*;
use agent_model::{ModelCallContext, ModelEvent, ModelRequest};
use agent_openai_compatible::{Transport, TransportFuture, TransportRequest, TransportResponse};
use futures_util::StreamExt;
use std::sync::Mutex;

struct CaptureTransport {
    requests: Mutex<Vec<TransportRequest>>,
    body: &'static str,
}
impl Transport for CaptureTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async move {
            Ok(TransportResponse {
                status: 200,
                headers: vec![],
                body: Box::pin(futures_util::stream::once(async {
                    Ok(self.body.as_bytes().to_vec())
                })),
            })
        })
    }
}

#[tokio::test]
async fn both_protocols_emit_bound_identity_and_real_product_version() {
    let home = tempfile::tempdir().unwrap();
    let factory = HostModelServiceFactory::new(home.path());
    let provider = agent_types::ProviderId::new("opencode").unwrap();
    let capabilities = assistant_runtime::ResolvedModelCapabilities {
        image_input: false,
        reasoning: None,
        tool_calls: false,
        tool_image_projection: agent_model::ToolImageProjection::Unsupported,
        tool_choice: agent_model::ToolChoiceCapabilities::default(),
        streaming: true,
    };
    for protocol in [
        ModelProtocol::OpenAiChatCompletions,
        ModelProtocol::OpenAiResponses,
    ] {
        let sse = match protocol {
            ModelProtocol::OpenAiChatCompletions => concat!(
                "data: {\"id\":\"c1\",\"model\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c1\",\"model\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            ),
            ModelProtocol::OpenAiResponses => include_str!(
                "../../../../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse"
            ),
        };
        let transport = Arc::new(CaptureTransport {
            requests: Mutex::new(vec![]),
            body: sse,
        });
        let service = factory
            .create_with_transport(
                ModelServiceFactoryRequest {
                    conversation_routing_id: Some("stable-owner-1"),
                    context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                    server_history_binding: None,
                    provider_state_binding: None,
                    input_token_count_supported: false,
                    compaction_threshold_tokens: std::num::NonZeroU64::new(1024).unwrap(),
                    external_configuration: None,
                    provider: &provider,
                    protocol,
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
        for _ in 0..2 {
            let request = ModelRequest {
                system: Default::default(),
                conversation: Default::default(),
                tools: vec![],
                tool_choice: agent_types::ToolChoice::Auto,
                generation: Default::default(),
                reasoning: None,
                provider_options: Default::default(),
            };
            let events = service
                .stream(request, ModelCallContext::default())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert!(
                matches!(events.last(), Some(ModelEvent::TurnFinished { .. })),
                "{events:?}"
            );
        }
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert!(
                request
                    .headers
                    .contains(&("x-opencode-session".into(), "stable-owner-1".into()))
            );
            assert!(request.headers.contains(&(
                "user-agent".into(),
                format!("ez-assistant/{}", assistant_protocol::SOFTWARE_VERSION)
            )));
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["model"], "fixture");
            assert_eq!(body["stream"], true);
            assert!(body.get("x-opencode-session").is_none());
            if protocol == ModelProtocol::OpenAiResponses {
                assert_eq!(body["store"], false);
                assert!(body.get("previous_response_id").is_none());
            }
        }
        assert_eq!(requests[0].body, requests[1].body);
    }
}
