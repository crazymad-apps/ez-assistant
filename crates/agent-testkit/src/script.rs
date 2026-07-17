//! 脚本化的 [`ModelService`] 实现：按预定脚本完成 Turn、建立前失败或流中失败。

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};

use agent_model::{
    ModelCallContext, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelRequest,
    ModelService, ModelStreamFuture,
};
use agent_types::{AssistantMessage, AssistantPart};
use futures_util::Stream;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
/// 一次脚本化的模型调用行为。
pub enum ModelScript {
    /// 建立前失败：`stream` 直接返回 `Err`，不产生任何事件。
    FailEstablishment(ModelError),
    /// 原样回放事件序列。流中失败用 `TurnFailed` 结尾表达；
    /// 畸形序列（缺 start、重复 finish、无终态等）也由此注入，
    /// 配合 `LifecycleValidator` 验证消费方的契约处理。
    Events(Vec<ModelEvent>),
}

/// 只通过 `ModelService` SPI 工作的确定性 Fake。
///
/// 每次 `stream` 调用按顺序弹出一条脚本；调用次数超过脚本数时以
/// 建立前 `ModelError::Config` 失败，提醒测试补脚本。
pub struct ScriptedModelService {
    capabilities: ModelCapabilities,
    scripts: Mutex<VecDeque<ModelScript>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModelService {
    /// 用给定能力声明和脚本队列创建服务。
    pub fn new(
        capabilities: ModelCapabilities,
        scripts: impl IntoIterator<Item = ModelScript>,
    ) -> Self {
        Self {
            capabilities,
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 便捷构造：单脚本，正常完成一个由完整消息描述的 Turn。
    pub fn completing(capabilities: ModelCapabilities, message: AssistantMessage) -> Self {
        Self::new(
            capabilities,
            [ModelScript::Events(message_events(&message))],
        )
    }

    /// 取出已收到的全部规范请求（按到达顺序），用于断言调用方行为。
    pub fn take_requests(&self) -> Vec<ModelRequest> {
        std::mem::take(
            &mut self
                .requests
                .lock()
                .expect("scripted service mutex poisoned"),
        )
    }
}

impl ModelService for ScriptedModelService {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let script = self
            .scripts
            .lock()
            .expect("scripted service mutex poisoned")
            .pop_front();
        self.requests
            .lock()
            .expect("scripted service mutex poisoned")
            .push(request);
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                // 建立前取消：以 Err 受控结束，不产生任何事件。
                return Err(ModelError::Cancelled);
            }
            match script {
                None => Err(ModelError::Config(
                    "scripted model service received more calls than scripted".to_owned(),
                )),
                Some(ModelScript::FailEstablishment(error)) => Err(error),
                Some(ModelScript::Events(events)) => Ok(Box::pin(ScriptedEventStream::new(
                    events,
                    context.cancellation,
                )) as ModelEventStream),
            }
        })
    }
}

/// 从完整的 [`AssistantMessage`] 生成规范事件序列。
///
/// reasoning/text 以单 delta 回放；tool call 的 arguments 以单分片回放；
/// `OpaqueProviderState` 没有流事件，只出现在最终消息中。
pub fn message_events(message: &AssistantMessage) -> Vec<ModelEvent> {
    let mut events = Vec::new();
    events.push(ModelEvent::TurnStarted {
        message_id: message.id.clone(),
        model: message.model.clone(),
    });
    for part in &message.parts {
        match part {
            AssistantPart::Reasoning(part) => {
                events.push(ModelEvent::ReasoningStarted {
                    id: part.id.clone(),
                });
                events.push(ModelEvent::ReasoningDelta {
                    id: part.id.clone(),
                    delta: part.text.clone(),
                });
                events.push(ModelEvent::ReasoningFinished {
                    id: part.id.clone(),
                });
            }
            AssistantPart::Text(part) => {
                events.push(ModelEvent::TextStarted {
                    id: part.id.clone(),
                });
                events.push(ModelEvent::TextDelta {
                    id: part.id.clone(),
                    delta: part.text.clone(),
                });
                events.push(ModelEvent::TextFinished {
                    id: part.id.clone(),
                });
            }
            AssistantPart::ToolCall(call) => {
                events.push(ModelEvent::ToolCallStarted {
                    id: call.id.clone(),
                    name: call.name.clone(),
                });
                events.push(ModelEvent::ToolCallDelta {
                    id: call.id.clone(),
                    arguments_delta: call.arguments.to_string(),
                });
                events.push(ModelEvent::ToolCallFinished {
                    id: call.id.clone(),
                    arguments: call.arguments.clone(),
                });
            }
            // opaque provider state 没有流事件，只出现在最终消息里。
            AssistantPart::ProviderState(_) => {}
        }
    }
    if let Some(usage) = &message.usage {
        events.push(ModelEvent::UsageUpdated {
            usage: usage.clone(),
        });
    }
    events.push(ModelEvent::TurnFinished {
        message: message.clone(),
    });
    events
}

/// 按脚本回放事件、并观察取消令牌的事件流。
struct ScriptedEventStream {
    events: VecDeque<ModelEvent>,
    cancellation: CancellationToken,
    terminated: bool,
}

impl ScriptedEventStream {
    fn new(events: Vec<ModelEvent>, cancellation: CancellationToken) -> Self {
        Self {
            events: events.into(),
            cancellation,
            terminated: false,
        }
    }
}

impl Stream for ScriptedEventStream {
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

#[cfg(test)]
mod tests {
    use agent_model::LifecycleValidator;
    use agent_types::{
        AssistantPart, ConversationSnapshot, FinishReason, MessageId, ModelIdentity, PartId,
        ProviderId, ReasoningPart, TextPart, TokenUsage, ToolCall, ToolCallId, ToolChoice,
        ToolName,
    };

    use super::*;
    use crate::EventCollector;

    fn capabilities() -> ModelCapabilities {
        ModelCapabilities {
            reasoning: true,
            tool_calls: true,
            streaming: true,
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
                cached_input_tokens: Some(4),
                reasoning_tokens: Some(3),
            }),
        }
    }

    fn sample_request() -> ModelRequest {
        ModelRequest {
            system: vec![],
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: agent_model::GenerationConfig::default(),
            reasoning: None,
            provider_options: agent_model::ProviderOptions::new(),
        }
    }

    #[tokio::test]
    async fn scripted_service_completes_text_reasoning_and_tool_call_turns() {
        let service = ScriptedModelService::completing(capabilities(), sample_message());
        let stream = service
            .stream(sample_request(), ModelCallContext::default())
            .await
            .expect("stream established");
        let collected = EventCollector::collect_validated(stream).await;
        assert_eq!(collected.assert_finished(), &sample_message());
        collected.assert_single_terminal();
    }

    #[tokio::test]
    async fn scripted_service_replays_establishment_and_in_stream_failures() {
        let service = ScriptedModelService::new(
            capabilities(),
            [
                ModelScript::FailEstablishment(ModelError::Auth("bad key".to_owned())),
                ModelScript::Events(vec![
                    ModelEvent::TurnStarted {
                        message_id: MessageId::new("message_1").expect("valid message id"),
                        model: ModelIdentity::new(
                            ProviderId::new("deepseek").expect("valid provider id"),
                            "deepseek-reasoner",
                        ),
                    },
                    ModelEvent::TurnFailed {
                        error: ModelError::RateLimited("slow down".to_owned()),
                    },
                ]),
            ],
        );
        let error = service
            .stream(sample_request(), ModelCallContext::default())
            .await
            .err()
            .expect("first call fails at establishment");
        assert_eq!(error, ModelError::Auth("bad key".to_owned()));

        let stream = service
            .stream(sample_request(), ModelCallContext::default())
            .await
            .expect("second call establishes");
        let collected = EventCollector::collect(stream).await;
        assert_eq!(
            collected.assert_failed(),
            &ModelError::RateLimited("slow down".to_owned())
        );
        // 两次调用的请求都被记录。
        assert_eq!(service.take_requests().len(), 2);
    }

    #[tokio::test]
    async fn scripted_service_rejects_calls_beyond_the_script() {
        let service = ScriptedModelService::new(capabilities(), []);
        let error = service
            .stream(sample_request(), ModelCallContext::default())
            .await
            .err()
            .expect("no script left");
        assert!(matches!(error, ModelError::Config(_)));
    }

    #[tokio::test]
    async fn malformed_scripts_are_replayed_verbatim_for_validator_testing() {
        // 注入缺 start 的 delta。
        let malformed = vec![
            ModelEvent::TurnStarted {
                message_id: MessageId::new("message_1").expect("valid message id"),
                model: ModelIdentity::new(
                    ProviderId::new("deepseek").expect("valid provider id"),
                    "deepseek-reasoner",
                ),
            },
            ModelEvent::TextDelta {
                id: PartId::new("text_1").expect("valid part id"),
                delta: "orphan".to_owned(),
            },
        ];
        let expected_len = malformed.len();
        let service =
            ScriptedModelService::new(capabilities(), [ModelScript::Events(malformed.clone())]);

        // 不套 validator：脚本原样回放（畸形序列确实到达消费方）。
        let stream = service
            .stream(sample_request(), ModelCallContext::default())
            .await
            .expect("stream established");
        let raw = EventCollector::collect(stream).await;
        assert_eq!(raw.events(), malformed.as_slice());

        // 套 validator：畸形序列被替换为唯一协议失败终态。
        let service = ScriptedModelService::new(capabilities(), [ModelScript::Events(malformed)]);
        let stream = service
            .stream(sample_request(), ModelCallContext::default())
            .await
            .expect("stream established");
        let collected =
            EventCollector::collect(Box::pin(LifecycleValidator::new(stream)) as ModelEventStream)
                .await;
        assert!(matches!(collected.assert_failed(), ModelError::Protocol(_)));
        assert!(collected.events().len() <= expected_len);
    }
}
