//! Responses Codec 与共享 HTTP/SSE Transport 的独立 [`ModelService`] 装配。

use std::sync::Arc;

use agent_model::{
    LifecycleValidator, ModelCallContext, ModelCapabilities, ModelError, ModelEvent,
    ModelEventStream, ModelRequest, ModelService, ModelStreamFuture, ModelTransportErrorKind,
    ToolImageProjection,
};
use futures_core::Stream;
use futures_util::StreamExt;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    BearerCredential, BodyStream, ReqwestTransport, SseParser, Transport, TransportError,
    TransportRequest, TransportTimeouts,
    shared::{join_endpoint, route_fingerprint, validate_base_url},
};

use super::{
    ResponsesProtocolAdapter, encode::encode_request_with_images, schema::ResponsesErrorBody,
    stream::ResponsesAssembler,
};

const ERROR_BODY_SAMPLE_LIMIT: usize = 2048;

#[derive(Debug, Error)]
pub enum OpenAiResponsesServiceError {
    #[error("invalid OpenAI-compatible base URL: {0}")]
    InvalidBaseUrl(&'static str),
    #[error("failed to construct OpenAI-compatible transport: {0}")]
    Transport(#[from] TransportError),
}

/// 一条构造期固定路由的 OpenAI Responses compatible 单 Turn 服务。
pub struct OpenAiResponsesService {
    base_url: String,
    credential: BearerCredential,
    model: String,
    context_window_tokens: u64,
    adapter: ResponsesProtocolAdapter,
    transport: Arc<dyn Transport>,
    capabilities: ModelCapabilities,
}

impl OpenAiResponsesService {
    pub fn new(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ResponsesProtocolAdapter,
        timeouts: TransportTimeouts,
    ) -> Result<Self, OpenAiResponsesServiceError> {
        let base_url = validate_base_url(base_url.into())
            .map_err(OpenAiResponsesServiceError::InvalidBaseUrl)?;
        let transport = Arc::new(ReqwestTransport::with_timeouts(timeouts)?);
        Ok(Self::build(
            base_url,
            credential,
            model,
            context_window_tokens,
            adapter,
            transport,
            None,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_capabilities(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ResponsesProtocolAdapter,
        capabilities: ModelCapabilities,
        timeouts: TransportTimeouts,
    ) -> Result<Self, OpenAiResponsesServiceError> {
        let base_url = validate_base_url(base_url.into())
            .map_err(OpenAiResponsesServiceError::InvalidBaseUrl)?;
        let transport = Arc::new(ReqwestTransport::with_timeouts(timeouts)?);
        Ok(Self::build(
            base_url,
            credential,
            model,
            context_window_tokens,
            adapter,
            transport,
            Some(capabilities),
        ))
    }

    pub fn with_transport(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ResponsesProtocolAdapter,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, OpenAiResponsesServiceError> {
        let base_url = validate_base_url(base_url.into())
            .map_err(OpenAiResponsesServiceError::InvalidBaseUrl)?;
        Ok(Self::build(
            base_url,
            credential,
            model,
            context_window_tokens,
            adapter,
            transport,
            None,
        ))
    }

    fn build(
        base_url: String,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ResponsesProtocolAdapter,
        transport: Arc<dyn Transport>,
        capabilities: Option<ModelCapabilities>,
    ) -> Self {
        let model = model.into();
        let fingerprint = route_fingerprint(
            adapter.provider.as_str(),
            adapter.protocol.as_str(),
            &base_url,
            &model,
        );
        let adapter = adapter.bind_route(fingerprint);
        let capabilities = capabilities.unwrap_or_else(|| ModelCapabilities {
            reasoning: !adapter.reasoning_effort_values.is_empty(),
            image_input: false,
            tool_calls: true,
            multimodal_tool_result: adapter.tool_image_projection
                != ToolImageProjection::Unsupported,
            tool_choice: adapter.tool_choice,
            streaming: true,
        });
        Self {
            base_url,
            credential,
            model,
            context_window_tokens,
            adapter,
            transport,
            capabilities,
        }
    }

    fn responses_url(&self) -> String {
        join_endpoint(&self.base_url, "responses")
    }
}

impl ModelService for OpenAiResponsesService {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let encoded = encode_request_with_images(
                &request,
                &context.prepared_images,
                &self.adapter,
                &self.model,
            )?;
            let body = serde_json::to_vec(&encoded).map_err(|error| {
                ModelError::Config(format!(
                    "failed to serialize the encoded Responses request: {error}"
                ))
            })?;
            let transport_request = TransportRequest {
                trace: context.trace.clone(),
                method: "POST".to_owned(),
                url: self.responses_url(),
                headers: vec![
                    ("content-type".to_owned(), "application/json".to_owned()),
                    ("accept".to_owned(), "text/event-stream".to_owned()),
                    (
                        "authorization".to_owned(),
                        self.credential.authorization_header(),
                    ),
                ],
                body,
            };
            let response = tokio::select! {
                result = self.transport.execute(transport_request) => result,
                () = context.cancellation.cancelled() => return Err(ModelError::Cancelled),
            }
            .map_err(map_transport_error)?;
            if !(200..300).contains(&response.status) {
                let status = response.status;
                let headers = response.headers;
                let sample = read_error_sample(response.body, &context.cancellation).await?;
                return Err(classify_http_error(status, &headers, &sample));
            }
            let events = event_stream(
                response.body,
                self.adapter.clone(),
                self.model.clone(),
                context.cancellation,
            );
            Ok(Box::pin(LifecycleValidator::new(Box::pin(events))) as ModelEventStream)
        })
    }
}

async fn read_error_sample(
    body: BodyStream,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ModelError> {
    let read = async move {
        let mut sample = Vec::new();
        let mut body = body;
        while sample.len() < ERROR_BODY_SAMPLE_LIMIT {
            match body.next().await {
                Some(Ok(chunk)) => {
                    let remaining = ERROR_BODY_SAMPLE_LIMIT - sample.len();
                    sample.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                }
                _ => break,
            }
        }
        sample
    };
    tokio::select! {
        sample = read => Ok(sample),
        () = cancellation.cancelled() => Err(ModelError::Cancelled),
    }
}

fn classify_http_error(status: u16, headers: &[(String, String)], sample: &[u8]) -> ModelError {
    let body = serde_json::from_slice::<ResponsesErrorBody>(sample).ok();
    let message = body.as_ref().map(|body| body.error.message.clone());
    let retry_after_ms = parse_retry_after_ms(headers);
    match status {
        401 | 403 => ModelError::Auth(message.unwrap_or_else(|| {
            format!("provider rejected the request as unauthorized (status {status})")
        })),
        429 => ModelError::RateLimited {
            message: message
                .unwrap_or_else(|| format!("provider rate limited the request (status {status})")),
            retry_after_ms,
        },
        408 | 425 | 500..=599 => ModelError::Unavailable {
            message: message
                .unwrap_or_else(|| format!("provider temporarily unavailable (status {status})")),
            status: Some(status),
            retry_after_ms,
        },
        _ if body.as_ref().is_some_and(|body| {
            matches!(
                body.error.code.as_deref().or(body.error.kind.as_deref()),
                Some("context_length_exceeded")
            )
        }) =>
        {
            ModelError::ContextOverflow {
                message: message.unwrap_or_else(|| "request exceeds the context window".to_owned()),
            }
        }
        _ => ModelError::Provider {
            message: message.unwrap_or_else(|| {
                format!("provider returned status {status} without a structured error body")
            }),
            status: Some(status),
        },
    }
}

fn parse_retry_after_ms(headers: &[(String, String)]) -> Option<u64> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1_000))
}

fn map_transport_error(error: TransportError) -> ModelError {
    let (kind, message) = match error {
        TransportError::Connect(message) => (ModelTransportErrorKind::Connection, message),
        TransportError::Timeout => (
            ModelTransportErrorKind::Timeout,
            "request timed out".to_owned(),
        ),
        TransportError::Interrupted(message) => (ModelTransportErrorKind::Interrupted, message),
    };
    ModelError::Transport { kind, message }
}

fn event_stream(
    body: BodyStream,
    adapter: ResponsesProtocolAdapter,
    model: String,
    cancellation: CancellationToken,
) -> impl Stream<Item = ModelEvent> + Send {
    async_stream::stream! {
        let mut body = body;
        let mut parser = SseParser::new();
        let mut assembler = ResponsesAssembler::new(adapter, model);
        let mut pending_terminal: Option<Vec<ModelEvent>> = None;
        let mut saw_done_marker = false;
        while !saw_done_marker {
            if cancellation.is_cancelled() {
                yield ModelEvent::TurnFailed { error: ModelError::Cancelled };
                return;
            }
            let next = tokio::select! {
                item = body.next() => Some(item),
                () = cancellation.cancelled() => None,
            };
            let Some(item) = next else {
                yield ModelEvent::TurnFailed { error: ModelError::Cancelled };
                return;
            };
            let bytes = match item {
                Some(Ok(bytes)) => bytes,
                Some(Err(error)) => {
                    yield ModelEvent::TurnFailed { error: map_transport_error(error) };
                    return;
                }
                None => break,
            };
            let frames = match parser.push(&bytes) {
                Ok(frames) => frames,
                Err(error) => {
                    yield ModelEvent::TurnFailed { error };
                    return;
                }
            };
            for frame in frames {
                if frame.data.trim() == "[DONE]" {
                    saw_done_marker = true;
                    break;
                }
                let value: serde_json::Value = match serde_json::from_str(&frame.data) {
                    Ok(value) => value,
                    Err(_) => {
                        yield ModelEvent::TurnFailed {
                            error: ModelError::Protocol(
                                "sse data frame is not a valid Responses event".to_owned(),
                            ),
                        };
                        return;
                    }
                };
                let events = match assembler.push(&value) {
                    Ok(events) => events,
                    Err(error) => {
                        yield ModelEvent::TurnFailed { error };
                        return;
                    }
                };
                if assembler.is_terminal() {
                    if pending_terminal.replace(events).is_some() {
                        yield ModelEvent::TurnFailed {
                            error: ModelError::Protocol(
                                "Responses stream emitted more than one terminal event".to_owned(),
                            ),
                        };
                        return;
                    }
                    continue;
                }
                for event in events {
                    if cancellation.is_cancelled() {
                        yield ModelEvent::TurnFailed { error: ModelError::Cancelled };
                        return;
                    }
                    yield event;
                }
            }
        }
        if cancellation.is_cancelled() {
            yield ModelEvent::TurnFailed { error: ModelError::Cancelled };
            return;
        }
        match (pending_terminal, assembler.finalize()) {
            (Some(events), Ok(_)) => {
                for event in events {
                    yield event;
                }
            }
            (_, Err(error)) => yield ModelEvent::TurnFailed { error },
            (None, Ok(_)) => {
                yield ModelEvent::TurnFailed {
                    error: ModelError::Protocol(
                        "Responses stream ended without a terminal event".to_owned(),
                    ),
                };
            }
        }
    }
}
