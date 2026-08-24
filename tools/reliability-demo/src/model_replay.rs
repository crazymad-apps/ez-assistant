//! 直接重现规范 ModelService 边界的离线 Model Replay。

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use agent_model::{
    LifecycleValidator, ModelCallContext, ModelCapabilities, ModelError, ModelEvent,
    ModelEventStream, ModelRequest, ModelService, ModelStreamFuture,
};
use futures_util::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::{
    replay::{
        RecordedModelCall, RecordedModelOutcome, ReplayError, adapter_from_metadata,
        capabilities_from_adapter, recorded_model_calls, request_mismatch_field,
    },
    trace::LoadedTrace,
};

/// 只持有不可变录制脚本，不连接 Provider、工具或 Journal。
pub(crate) struct ReplayModelService {
    scripts: Arc<Mutex<VecDeque<RecordedModelCall>>>,
    mismatch: Arc<Mutex<Option<ReplayError>>>,
    capabilities: ModelCapabilities,
    context_window_tokens: u64,
}

impl ReplayModelService {
    pub(crate) fn from_trace(trace: &LoadedTrace) -> Result<Self, ReplayError> {
        let protocol_adapter = adapter_from_metadata(&trace.started.provider)?;
        Ok(Self {
            scripts: Arc::new(Mutex::new(VecDeque::from(recorded_model_calls(trace)?))),
            mismatch: Arc::new(Mutex::new(None)),
            capabilities: capabilities_from_adapter(&protocol_adapter),
            context_window_tokens: trace.started.provider.context_window_tokens,
        })
    }

    fn set_mismatch(&self, error: ReplayError) {
        let mut mismatch = self
            .mismatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if mismatch.is_none() {
            *mismatch = Some(error);
        }
    }

    pub(crate) fn take_mismatch(&self) -> Option<ReplayError> {
        self.mismatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub(crate) fn remaining(&self) -> usize {
        self.scripts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl ModelService for ReplayModelService {
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

            let script = {
                let mut scripts = self
                    .scripts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(expected) = scripts.front() else {
                    drop(scripts);
                    self.set_mismatch(ReplayError::ScriptExhausted("model"));
                    return Err(ModelError::Config(
                        "model replay script is exhausted".into(),
                    ));
                };
                if let Some(field) = request_mismatch_field(&expected.request, &request) {
                    drop(scripts);
                    self.set_mismatch(ReplayError::RequestMismatch {
                        layer: "model",
                        field,
                    });
                    return Err(ModelError::Config(format!(
                        "model replay request mismatch in `{field}`"
                    )));
                }
                scripts
                    .pop_front()
                    .expect("front was checked while holding the same lock")
            };

            match script.outcome {
                RecordedModelOutcome::Failed(error) => Err(error),
                RecordedModelOutcome::Established => {
                    let stream = ReplayEventStream::new(script.events, context.cancellation);
                    Ok(Box::pin(LifecycleValidator::new(Box::pin(stream))) as ModelEventStream)
                }
            }
        })
    }
}

struct ReplayEventStream {
    events: VecDeque<ModelEvent>,
    cancellation: CancellationToken,
    terminated: bool,
}

impl ReplayEventStream {
    fn new(events: Vec<ModelEvent>, cancellation: CancellationToken) -> Self {
        Self {
            events: VecDeque::from(events),
            cancellation,
            terminated: false,
        }
    }
}

impl Stream for ReplayEventStream {
    type Item = ModelEvent;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }
        if this.cancellation.is_cancelled() {
            this.terminated = true;
            return Poll::Ready(Some(ModelEvent::TurnFailed {
                error: ModelError::Cancelled,
            }));
        }
        let event = this.events.pop_front();
        if event.as_ref().is_some_and(ModelEvent::is_terminal) {
            this.terminated = true;
        }
        Poll::Ready(event)
    }
}

/// CLI 使用相同录制请求逐个驱动 ReplayModelService，并比较原始边界结果。
pub(crate) async fn run_model_replay(trace: &LoadedTrace) -> Result<usize, ReplayError> {
    let calls = recorded_model_calls(trace)?;
    let service = ReplayModelService::from_trace(trace)?;
    for call in &calls {
        let result = service
            .stream(
                call.request.clone(),
                ModelCallContext {
                    cancellation: CancellationToken::new(),
                    trace: Some(agent_model::TraceContext::new(call.correlation_id.clone())),
                    prepared_images: Default::default(),
                },
            )
            .await;
        if let Some(error) = service.take_mismatch() {
            return Err(error);
        }
        match (&call.outcome, result) {
            (RecordedModelOutcome::Failed(expected), Err(actual)) if expected == &actual => {}
            (RecordedModelOutcome::Established, Ok(mut stream)) => {
                let mut actual = Vec::new();
                while let Some(event) = stream.next().await {
                    actual.push(event);
                }
                if actual != call.events {
                    return Err(ReplayError::ResultMismatch {
                        layer: "model",
                        correlation_id: call.correlation_id.clone(),
                    });
                }
            }
            _ => {
                return Err(ReplayError::ResultMismatch {
                    layer: "model",
                    correlation_id: call.correlation_id.clone(),
                });
            }
        }
    }
    if service.remaining() != 0 {
        return Err(ReplayError::CorruptScript(
            "model replay did not consume every logical call".into(),
        ));
    }
    Ok(calls.len())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use agent_types::{
        ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart, ToolCallId,
        ToolDefinition, ToolName, UserMessage, UserPart,
    };
    use serde_json::json;

    use super::*;
    use crate::replay::{
        RecordedModelOutcome,
        test_support::{CORRELATION, model_trace, request, text_events},
    };

    #[tokio::test]
    async fn model_replay_reproduces_success_and_establishment_failure() {
        let success = model_trace(RecordedModelOutcome::Established, text_events());
        assert_eq!(run_model_replay(&success).await.unwrap(), 1);

        let error = ModelError::Unavailable {
            message: "fixture unavailable".into(),
            status: Some(503),
            retry_after_ms: Some(1_000),
        };
        let failed = model_trace(RecordedModelOutcome::Failed(error), vec![]);
        assert_eq!(run_model_replay(&failed).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn model_replay_reports_each_request_dimension_without_consuming_script() {
        let trace = model_trace(RecordedModelOutcome::Established, text_events());
        let mutations: Vec<(&'static str, ModelRequest)> = vec![
            ("system", {
                let mut request = request();
                request.system = agent_model::SystemPromptSnapshot::new(vec!["changed".into()]);
                request
            }),
            ("conversation", {
                let mut request = request();
                request.conversation =
                    ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
                        origin: Default::default(),
                        transcript_visibility: Default::default(),
                        id: MessageId::new("user-1").unwrap(),
                        parts: vec![UserPart::Text(TextPart {
                            id: PartId::new("user-part-1").unwrap(),
                            text: "changed".into(),
                        })],
                    })]);
                request
            }),
            ("tools", {
                let mut request = request();
                request.tools.push(ToolDefinition {
                    name: ToolName::new("fixture_tool").unwrap(),
                    description: "fixture".into(),
                    input_schema: json!({"type": "object"}),
                });
                request
            }),
            ("provider_options", {
                let mut request = request();
                request
                    .provider_options
                    .insert("fixture", json!({"mode": "changed"}))
                    .unwrap();
                request
            }),
        ];

        for (expected_field, request) in mutations {
            let service = ReplayModelService::from_trace(&trace).unwrap();
            let Err(error) = service.stream(request, ModelCallContext::default()).await else {
                panic!("mismatched request unexpectedly established a stream")
            };
            assert!(matches!(error, ModelError::Config(_)));
            assert!(matches!(
                service.take_mismatch(),
                Some(ReplayError::RequestMismatch {
                    layer: "model",
                    field,
                }) if field == expected_field
            ));
            assert_eq!(service.remaining(), 1);
        }
    }

    #[tokio::test]
    async fn current_cancellation_stops_only_the_replay_call() {
        let trace = model_trace(RecordedModelOutcome::Established, text_events());
        let service = ReplayModelService::from_trace(&trace).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let Err(error) = service
            .stream(
                request(),
                ModelCallContext {
                    cancellation,
                    trace: None,
                    prepared_images: Default::default(),
                },
            )
            .await
        else {
            panic!("pre-cancelled replay unexpectedly established a stream")
        };
        assert_eq!(error, ModelError::Cancelled);
        assert_eq!(service.remaining(), 1);

        let cancellation = CancellationToken::new();
        let mut stream = service
            .stream(
                request(),
                ModelCallContext {
                    cancellation: cancellation.clone(),
                    trace: None,
                    prepared_images: Default::default(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            stream.next().await,
            Some(ModelEvent::TurnStarted { .. })
        ));
        cancellation.cancel();
        assert_eq!(
            stream.next().await,
            Some(ModelEvent::TurnFailed {
                error: ModelError::Cancelled
            })
        );
        assert!(stream.next().await.is_none());
        assert_eq!(service.remaining(), 0);
    }

    #[tokio::test]
    async fn tool_call_events_are_data_and_have_no_execution_path() {
        static TOOL_EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
        TOOL_EXECUTIONS.store(0, Ordering::SeqCst);
        let model = agent_types::ModelIdentity::new(
            agent_types::ProviderId::new("fixture").unwrap(),
            "fixture-model",
        );
        let events = vec![
            ModelEvent::TurnStarted {
                message_id: MessageId::new("tool-turn").unwrap(),
                model,
            },
            ModelEvent::ToolCallStarted {
                id: ToolCallId::new("tool-call-1").unwrap(),
                name: ToolName::new("dangerous_tool").unwrap(),
            },
            ModelEvent::ToolCallDelta {
                id: ToolCallId::new("tool-call-1").unwrap(),
                arguments_delta: "{}".into(),
            },
            ModelEvent::ToolCallFinished {
                id: ToolCallId::new("tool-call-1").unwrap(),
                arguments: json!({}),
            },
            ModelEvent::TurnFailed {
                error: ModelError::Cancelled,
            },
        ];
        let trace = model_trace(RecordedModelOutcome::Established, events);
        assert_eq!(run_model_replay(&trace).await.unwrap(), 1);
        assert_eq!(TOOL_EXECUTIONS.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn corrupt_model_event_script_is_not_reported_as_success() {
        let mut events = text_events();
        events.pop();
        let trace = model_trace(RecordedModelOutcome::Established, events);
        assert!(matches!(
            run_model_replay(&trace).await,
            Err(ReplayError::ResultMismatch {
                layer: "model",
                correlation_id,
            }) if correlation_id == CORRELATION
        ));
    }

    #[tokio::test]
    async fn model_replay_reports_script_exhaustion_after_exact_consumption() {
        let trace = model_trace(RecordedModelOutcome::Established, text_events());
        let service = ReplayModelService::from_trace(&trace).unwrap();
        let mut stream = service
            .stream(request(), ModelCallContext::default())
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let result = service.stream(request(), ModelCallContext::default()).await;
        assert!(matches!(result, Err(ModelError::Config(_))));
        assert!(matches!(
            service.take_mismatch(),
            Some(ReplayError::ScriptExhausted("model"))
        ));
    }
}
