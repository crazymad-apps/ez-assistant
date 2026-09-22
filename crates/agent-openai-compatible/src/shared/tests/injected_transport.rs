//! 显式能力与宿主 Transport 组合不改变 Codec，也不让可信拒绝进入通用重试。

use std::sync::atomic::{AtomicUsize, Ordering};

use agent_model::{ModelCapabilities, ModelRetryPolicy, ModelRetryReason, RetryingModelService};

use super::*;
use crate::{OpenAiResponsesService, ResponsesProtocolAdapter};

fn injected_service(
    responses: bool,
    capabilities: ModelCapabilities,
    transport: Arc<dyn Transport>,
) -> Arc<dyn ModelService> {
    if responses {
        Arc::new(
            OpenAiResponsesService::with_transport_and_capabilities(
                BASE_URL,
                BearerCredential::new(TOKEN),
                "gpt-test",
                128_000,
                ResponsesProtocolAdapter::openai(),
                capabilities,
                transport,
            )
            .expect("valid service"),
        )
    } else {
        Arc::new(
            OpenAiChatCompletionsService::with_transport_and_capabilities(
                BASE_URL,
                BearerCredential::new(TOKEN),
                "gpt-test",
                128_000,
                base_adapter(),
                capabilities,
                transport,
            )
            .expect("valid service"),
        )
    }
}

#[tokio::test]
async fn explicit_capabilities_preserve_both_protocols_wire_and_events() {
    for responses in [false, true] {
        let body = if responses {
            include_str!("../../../fixtures/responses/openai/text.sse")
        } else {
            OK_SSE_BODY
        };
        let plain_transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            200,
            body.as_bytes().to_vec(),
        ))]));
        let injected_transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
            200,
            body.as_bytes().to_vec(),
        ))]));
        let plain: Arc<dyn ModelService> = if responses {
            Arc::new(
                OpenAiResponsesService::with_transport(
                    BASE_URL,
                    BearerCredential::new(TOKEN),
                    "gpt-test",
                    128_000,
                    ResponsesProtocolAdapter::openai(),
                    plain_transport.clone(),
                )
                .expect("valid service"),
            )
        } else {
            Arc::new(service_with(&plain_transport))
        };
        let mut capabilities = plain.capabilities().clone();
        // 与 Adapter 默认推断不同，必须保留 Runtime 给定的能力事实。
        capabilities.image_input = true;
        let injected =
            injected_service(responses, capabilities.clone(), injected_transport.clone());
        assert_eq!(injected.capabilities(), &capabilities);
        let mut events = Vec::new();
        for service in [plain, injected] {
            let stream = service
                .stream(simple_request(), ModelCallContext::default())
                .await
                .expect("stream");
            events.push(stream.collect::<Vec<_>>().await);
        }
        assert_eq!(events[0], events[1]);
        assert_eq!(
            plain_transport.take_requests(),
            injected_transport.take_requests()
        );
    }
}

struct RejectingTransport {
    calls: AtomicUsize,
    status: u16,
}

impl Transport for RejectingTransport {
    fn execute<'a>(&'a self, _request: TransportRequest) -> TransportFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Err(TransportError::Rejected(ModelError::Provider {
                message: "request rejected by trusted transport policy".to_owned(),
                status: Some(self.status),
            }))
        })
    }
}

#[tokio::test]
async fn trusted_rejections_never_resend_even_for_service_unavailable() {
    for responses in [false, true] {
        for status in [401, 409, 503] {
            let transport = Arc::new(RejectingTransport {
                calls: AtomicUsize::new(0),
                status,
            });
            let service = RetryingModelService::new(
                injected_service(responses, ModelCapabilities::default(), transport.clone()),
                ModelRetryPolicy::new(
                    [
                        ModelRetryReason::Connection,
                        ModelRetryReason::Timeout,
                        ModelRetryReason::Unavailable,
                        ModelRetryReason::RateLimited,
                    ]
                    .into_iter()
                    .collect(),
                    vec![Duration::ZERO; 2],
                    Duration::ZERO,
                ),
            );
            let result = service
                .stream(simple_request(), ModelCallContext::default())
                .await;
            assert!(
                matches!(result, Err(ModelError::Provider { status: Some(value), .. }) if value == status)
            );
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        }
    }
}
