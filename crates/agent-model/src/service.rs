use std::{future::Future, pin::Pin};

use futures_core::Stream;

use crate::{ModelCallContext, ModelCapabilities, ModelError, ModelEvent, ModelRequest};

/// 建立一次模型流的 Future。
///
/// `Err` 表示流建立前失败（配置、认证、连接等），调用方不会得到任何事件；
/// `Ok` 之后发生的失败以 [`ModelEvent::TurnFailed`] 受控终态表达。
pub type ModelStreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelEventStream, ModelError>> + Send + 'a>>;

/// 成功建立的规范模型事件流。
///
/// 契约上恰好产生一个终态事件（`TurnFinished` 或 `TurnFailed`）；调用方可以用
/// [`crate::LifecycleValidator`] 对不信任的生产方强制执行该契约。
pub type ModelEventStream = Pin<Box<dyn Stream<Item = ModelEvent> + Send>>;

/// Provider-neutral 的单次模型 Turn 服务。
///
/// 实现者一次只执行一个 Provider Turn：不执行工具，也不会在收到 Tool Call 后
/// 自动发起下一次调用；是否继续由 Agent Engine 显式决定。
///
/// credential 在实现构造时注入，不得出现在请求、上下文、事件和 Debug 输出中。
pub trait ModelService: Send + Sync {
    /// 该服务对单次 Turn 的能力声明。
    fn capabilities(&self) -> &ModelCapabilities;

    /// 调用方为当前服务绑定模型配置的上下文窗口上限。
    fn context_window_tokens(&self) -> u64;

    /// 发起一次 Provider Turn。
    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_>;
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        pin::Pin,
        task::{Context, Poll},
    };

    use agent_types::{
        AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, PartId,
        ProviderId, ReasoningPart, TextPart, TokenUsage, ToolCall, ToolCallId, ToolName,
    };
    use futures_core::Stream;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        LifecycleValidator,
        testutil::{block_on, collect, next},
    };

    /// 只通过 `ModelService` SPI 完成一次模型 Turn 的确定性 Fake。
    struct FakeModelService {
        capabilities: ModelCapabilities,
        context_window_tokens: u64,
        behavior: FakeBehavior,
    }

    enum FakeBehavior {
        /// 正常完成一个 Turn。
        Complete(AssistantMessage),
        /// 建立前失败。
        FailEstablishment(ModelError),
        /// 建立后流中失败。
        FailInStream(ModelError),
    }

    impl ModelService for FakeModelService {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            self.context_window_tokens
        }

        fn stream(
            &self,
            _request: ModelRequest,
            context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(async move {
                if context.cancellation.is_cancelled() {
                    // 建立前取消：以 Err 受控结束，不产生任何事件。
                    return Err(ModelError::Cancelled);
                }
                match &self.behavior {
                    FakeBehavior::FailEstablishment(error) => Err(error.clone()),
                    FakeBehavior::Complete(message) => Ok(Box::pin(FakeEventStream::new(
                        events_for(message),
                        context.cancellation,
                    ))
                        as ModelEventStream),
                    FakeBehavior::FailInStream(error) => {
                        let mut events = VecDeque::new();
                        events.push_back(started_event());
                        events.push_back(ModelEvent::TurnFailed {
                            error: error.clone(),
                        });
                        Ok(Box::pin(FakeEventStream::new(events, context.cancellation))
                            as ModelEventStream)
                    }
                }
            })
        }
    }

    /// 按脚本回放事件、并观察取消的 Fake 事件流。
    struct FakeEventStream {
        events: VecDeque<ModelEvent>,
        cancellation: CancellationToken,
        terminated: bool,
    }

    impl FakeEventStream {
        fn new(events: VecDeque<ModelEvent>, cancellation: CancellationToken) -> Self {
            Self {
                events,
                cancellation,
                terminated: false,
            }
        }
    }

    impl Stream for FakeEventStream {
        type Item = ModelEvent;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<ModelEvent>> {
            let this = self.get_mut();
            if this.terminated {
                // 自己的终态之后不再产生任何事件。
                return Poll::Ready(None);
            }
            if this.cancellation.is_cancelled() {
                // 流中取消：以唯一 TurnFailed 受控结束。
                this.terminated = true;
                return Poll::Ready(Some(ModelEvent::TurnFailed {
                    error: ModelError::Cancelled,
                }));
            }
            Poll::Ready(this.events.pop_front())
        }
    }

    fn started_event() -> ModelEvent {
        ModelEvent::TurnStarted {
            message_id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
        }
    }

    fn sample_message() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
            parts: vec![
                AssistantPart::Reasoning(ReasoningPart {
                    id: PartId::new("reasoning_1").expect("valid part id"),
                    text: "Need the date first".to_owned(),
                }),
                AssistantPart::Text(TextPart {
                    id: PartId::new("text_1").expect("valid part id"),
                    text: "Let me check".to_owned(),
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: ToolCallId::new("call_1").expect("valid call id"),
                    name: ToolName::new("get_date").expect("valid tool name"),
                    arguments: serde_json::json!({}),
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                cached_input_tokens: None,
                reasoning_tokens: Some(3),
            }),
        }
    }

    fn events_for(message: &AssistantMessage) -> VecDeque<ModelEvent> {
        let mut events = VecDeque::new();
        events.push_back(started_event());
        for part in &message.parts {
            match part {
                AssistantPart::Reasoning(part) => {
                    events.push_back(ModelEvent::ReasoningStarted {
                        id: part.id.clone(),
                    });
                    events.push_back(ModelEvent::ReasoningDelta {
                        id: part.id.clone(),
                        delta: part.text.clone(),
                    });
                    events.push_back(ModelEvent::ReasoningFinished {
                        id: part.id.clone(),
                    });
                }
                AssistantPart::Text(part) => {
                    events.push_back(ModelEvent::TextStarted {
                        id: part.id.clone(),
                    });
                    events.push_back(ModelEvent::TextDelta {
                        id: part.id.clone(),
                        delta: part.text.clone(),
                    });
                    events.push_back(ModelEvent::TextFinished {
                        id: part.id.clone(),
                    });
                }
                AssistantPart::ToolCall(call) => {
                    events.push_back(ModelEvent::ToolCallStarted {
                        id: call.id.clone(),
                        name: call.name.clone(),
                    });
                    events.push_back(ModelEvent::ToolCallDelta {
                        id: call.id.clone(),
                        arguments_delta: call.arguments.to_string(),
                    });
                    events.push_back(ModelEvent::ToolCallFinished {
                        id: call.id.clone(),
                        arguments: call.arguments.clone(),
                    });
                }
                // opaque provider state 没有流事件，只出现在最终消息里。
                AssistantPart::ProviderState(_) => {}
            }
        }
        if let Some(usage) = &message.usage {
            events.push_back(ModelEvent::UsageUpdated {
                usage: usage.clone(),
            });
        }
        events.push_back(ModelEvent::TurnFinished {
            message: message.clone(),
        });
        events
    }

    fn sample_request() -> ModelRequest {
        ModelRequest {
            system: crate::SystemPromptSnapshot::default(),
            conversation: agent_types::ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: agent_types::ToolChoice::Auto,
            generation: crate::GenerationConfig::default(),
            reasoning: None,
            provider_options: crate::ProviderOptions::new(),
        }
    }

    fn fake_service(behavior: FakeBehavior) -> FakeModelService {
        FakeModelService {
            capabilities: ModelCapabilities {
                reasoning: true,
                image_input: false,
                tool_calls: true,
                multimodal_tool_result: false,
                tool_choice: crate::ToolChoiceCapabilities::auto_only(),
                streaming: true,
            },
            context_window_tokens: 128_000,
            behavior,
        }
    }

    #[test]
    fn fake_service_completes_a_model_turn_through_the_spi() {
        let service = fake_service(FakeBehavior::Complete(sample_message()));
        assert!(service.capabilities().reasoning);
        assert_eq!(service.context_window_tokens(), 128_000);
        let stream = block_on(service.stream(sample_request(), ModelCallContext::default()))
            .expect("stream established");
        let events = collect(LifecycleValidator::new(stream));
        let Some(ModelEvent::TurnFinished { message }) = events.last() else {
            panic!("turn must finish with the assembled message");
        };
        assert_eq!(*message, sample_message());
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    }

    #[test]
    fn establishment_failure_differs_from_in_stream_failure() {
        // 建立前失败：stream 直接返回 Err，没有任何事件。
        let service = fake_service(FakeBehavior::FailEstablishment(ModelError::Auth(
            "invalid api key".to_owned(),
        )));
        let result = block_on(service.stream(sample_request(), ModelCallContext::default()));
        assert_eq!(
            result.err().expect("establishment must fail"),
            ModelError::Auth("invalid api key".to_owned())
        );

        // 建立后失败：先得到流，再以 TurnFailed 终态受控结束。
        let service = fake_service(FakeBehavior::FailInStream(ModelError::Provider {
            message: "upstream rejected the request".to_owned(),
            status: Some(400),
        }));
        let stream = block_on(service.stream(sample_request(), ModelCallContext::default()))
            .expect("stream established");
        let events = collect(LifecycleValidator::new(stream));
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ModelEvent::TurnStarted { .. }));
        assert_eq!(
            events[1],
            ModelEvent::TurnFailed {
                error: ModelError::Provider {
                    message: "upstream rejected the request".to_owned(),
                    status: Some(400),
                }
            }
        );
    }

    #[test]
    fn cancellation_before_establishment_returns_err() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let service = fake_service(FakeBehavior::Complete(sample_message()));
        let context = ModelCallContext::new(cancellation);
        let result = block_on(service.stream(sample_request(), context));
        assert_eq!(
            result.err().expect("cancelled before establishment"),
            ModelError::Cancelled
        );
    }

    #[test]
    fn cancellation_during_stream_yields_a_single_controlled_result() {
        let cancellation = CancellationToken::new();
        let service = fake_service(FakeBehavior::Complete(sample_message()));
        let context = ModelCallContext::new(cancellation.clone());
        let mut stream =
            block_on(service.stream(sample_request(), context)).expect("stream established");

        assert!(matches!(
            next(&mut stream),
            Some(ModelEvent::TurnStarted { .. })
        ));
        // 在 reasoning 中途取消。
        cancellation.cancel();
        assert_eq!(
            next(&mut stream),
            Some(ModelEvent::TurnFailed {
                error: ModelError::Cancelled
            })
        );
        // 受控终态之后流立即结束，不再有第二个结果。
        assert_eq!(next(&mut stream), None);
    }
}
