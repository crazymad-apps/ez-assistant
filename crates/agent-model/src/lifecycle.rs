use std::{
    collections::HashSet,
    pin::Pin,
    task::{Context, Poll},
};

use agent_types::{MessageId, PartId, ToolCallId};
use futures_core::Stream;

use crate::{ModelError, ModelEvent, ModelEventStream};

/// 流生命周期验证器。
///
/// 包装任意 [`ModelEventStream`]，把不信任的生产方输出强制约束到规范事件契约：
///
/// - 首个事件必须是 `TurnStarted`；`TurnFailed` 在任何阶段都是合法终态。
/// - Part 的 `Started`/`Delta`/`Finished` 按 ID 配对；不同 Part 允许交错。
/// - `TurnFinished` 到达时不允许存在未结束的 Part，且消息 ID 必须与
///   `TurnStarted` 声明的一致。
/// - 恰好一个终态：内部流结束仍无终态时，补发 `TurnFailed(Protocol)`；
///   终态之后到达的数据一律丢弃，不再产生业务事件。
/// - 任何契约违反都以唯一 `TurnFailed(Protocol)` 终态替换，违规事件本身不下发。
pub struct LifecycleValidator {
    inner: ModelEventStream,
    state: ValidatorState,
    open_parts: HashSet<OpenPart>,
    message_id: Option<MessageId>,
}

impl LifecycleValidator {
    /// 包装一个已建立的模型事件流。
    pub fn new(inner: ModelEventStream) -> Self {
        Self {
            inner,
            state: ValidatorState::AwaitingStart,
            open_parts: HashSet::new(),
            message_id: None,
        }
    }

    fn validate(&mut self, event: &ModelEvent) -> Result<(), ModelError> {
        match event {
            ModelEvent::TurnStarted { message_id, .. } => {
                if self.state != ValidatorState::AwaitingStart {
                    return Err(protocol("duplicate turn start"));
                }
                self.state = ValidatorState::Streaming;
                self.message_id = Some(message_id.clone());
                Ok(())
            }
            ModelEvent::ReasoningStarted { id } => self.open_part(OpenPart::Reasoning(id.clone())),
            ModelEvent::ReasoningDelta { id, .. } => {
                self.part_delta(&OpenPart::Reasoning(id.clone()))
            }
            ModelEvent::ReasoningFinished { id } => {
                self.finish_part(&OpenPart::Reasoning(id.clone()))
            }
            ModelEvent::TextStarted { id } => self.open_part(OpenPart::Text(id.clone())),
            ModelEvent::TextDelta { id, .. } => self.part_delta(&OpenPart::Text(id.clone())),
            ModelEvent::TextFinished { id } => self.finish_part(&OpenPart::Text(id.clone())),
            ModelEvent::ToolCallStarted { id, .. } => {
                self.open_part(OpenPart::ToolCall(id.clone()))
            }
            ModelEvent::ToolCallDelta { id, .. } => {
                self.part_delta(&OpenPart::ToolCall(id.clone()))
            }
            ModelEvent::ToolCallFinished { id, .. } => {
                self.finish_part(&OpenPart::ToolCall(id.clone()))
            }
            ModelEvent::UsageUpdated { .. } => self.require_streaming("usage update"),
            ModelEvent::TurnFinished { message } => {
                self.require_streaming("turn finish")?;
                if let Some(part) = self.open_parts.iter().next() {
                    return Err(protocol(format!(
                        "turn finished while {} is still open",
                        part.kind()
                    )));
                }
                if self.message_id.as_ref() != Some(&message.id) {
                    return Err(protocol(
                        "turn finished with a message id different from turn start",
                    ));
                }
                Ok(())
            }
            // 受控失败在任何阶段都是合法终态。
            ModelEvent::TurnFailed { .. } => Ok(()),
        }
    }

    fn require_streaming(&self, what: &'static str) -> Result<(), ModelError> {
        if self.state != ValidatorState::Streaming {
            return Err(protocol(format!("{what} arrived before turn start")));
        }
        Ok(())
    }

    fn open_part(&mut self, part: OpenPart) -> Result<(), ModelError> {
        self.require_streaming("part start")?;
        if !self.open_parts.insert(part.clone()) {
            return Err(protocol(format!("duplicate {} start", part.kind())));
        }
        Ok(())
    }

    fn part_delta(&mut self, part: &OpenPart) -> Result<(), ModelError> {
        self.require_streaming("part delta")?;
        if !self.open_parts.contains(part) {
            return Err(protocol(format!(
                "{} delta without an open matching part",
                part.kind()
            )));
        }
        Ok(())
    }

    fn finish_part(&mut self, part: &OpenPart) -> Result<(), ModelError> {
        self.require_streaming("part finish")?;
        if !self.open_parts.remove(part) {
            return Err(protocol(format!(
                "{} finish without an open matching part",
                part.kind()
            )));
        }
        Ok(())
    }
}

impl Stream for LifecycleValidator {
    type Item = ModelEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ModelEvent>> {
        let this = self.get_mut();
        if matches!(
            this.state,
            ValidatorState::Terminated | ValidatorState::Done
        ) {
            // 终态已发出：之后的 Provider 数据视为协议错误，不再产生业务事件。
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                // 内部流结束但缺少终态：补发受控协议失败，保证唯一终态。
                this.state = ValidatorState::Done;
                Poll::Ready(Some(ModelEvent::TurnFailed {
                    error: protocol("model stream ended without a terminal event"),
                }))
            }
            Poll::Ready(Some(event)) => match this.validate(&event) {
                Ok(()) => {
                    if event.is_terminal() {
                        this.state = ValidatorState::Terminated;
                    }
                    Poll::Ready(Some(event))
                }
                Err(error) => {
                    this.state = ValidatorState::Terminated;
                    Poll::Ready(Some(ModelEvent::TurnFailed { error }))
                }
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 验证器所处的流阶段。
enum ValidatorState {
    /// 尚未收到 `TurnStarted`。
    AwaitingStart,
    /// 正常流式阶段。
    Streaming,
    /// 终态已发出。
    Terminated,
    /// 流已结束。
    Done,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
/// 一个已开始但未结束的 Part。
enum OpenPart {
    Reasoning(PartId),
    Text(PartId),
    ToolCall(ToolCallId),
}

impl OpenPart {
    fn kind(&self) -> &'static str {
        match self {
            OpenPart::Reasoning(_) => "reasoning part",
            OpenPart::Text(_) => "text part",
            OpenPart::ToolCall(_) => "tool call",
        }
    }
}

fn protocol(message: impl Into<String>) -> ModelError {
    ModelError::Protocol(message.into())
}

#[cfg(test)]
mod tests {
    use agent_types::{
        AssistantMessage, FinishReason, MessageId, ModelIdentity, ProviderId, TokenUsage,
    };
    use futures_util::stream;

    use super::*;
    use crate::testutil::collect;

    fn part_id(value: &str) -> PartId {
        PartId::new(value).expect("valid part id")
    }

    fn call_id(value: &str) -> ToolCallId {
        ToolCallId::new(value).expect("valid call id")
    }

    fn started() -> ModelEvent {
        ModelEvent::TurnStarted {
            message_id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
        }
    }

    fn finished(parts: Vec<agent_types::AssistantPart>) -> ModelEvent {
        ModelEvent::TurnFinished {
            message: AssistantMessage {
                id: MessageId::new("message_1").expect("valid message id"),
                model: ModelIdentity::new(
                    ProviderId::new("deepseek").expect("valid provider id"),
                    "deepseek-reasoner",
                ),
                parts,
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    total_tokens: 2,
                    cached_input_tokens: None,
                    reasoning_tokens: None,
                }),
            },
        }
    }

    fn validate(events: Vec<ModelEvent>) -> Vec<ModelEvent> {
        collect(LifecycleValidator::new(Box::pin(stream::iter(events))))
    }

    fn expect_single_protocol_failure(events: &[ModelEvent]) -> String {
        assert_eq!(events.len(), 1);
        let [
            ModelEvent::TurnFailed {
                error: ModelError::Protocol(message),
            },
        ] = events
        else {
            panic!("expected a single protocol failure, got {events:?}");
        };
        message.clone()
    }

    #[test]
    fn accepts_a_plain_text_turn() {
        let events = vec![
            started(),
            ModelEvent::TextStarted {
                id: part_id("text_1"),
            },
            ModelEvent::TextDelta {
                id: part_id("text_1"),
                delta: "Hello".to_owned(),
            },
            ModelEvent::TextDelta {
                id: part_id("text_1"),
                delta: ", world".to_owned(),
            },
            ModelEvent::TextFinished {
                id: part_id("text_1"),
            },
            ModelEvent::UsageUpdated {
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 2,
                    total_tokens: 3,
                    cached_input_tokens: None,
                    reasoning_tokens: None,
                },
            },
            finished(vec![]),
        ];
        let expected_len = events.len();
        let output = validate(events);
        assert_eq!(output.len(), expected_len);
        assert!(matches!(
            output.last(),
            Some(ModelEvent::TurnFinished { .. })
        ));
    }

    #[test]
    fn accepts_reasoning_and_interleaved_tool_calls() {
        // 两个 Tool Call 的 arguments 分片交错到达，按 ID 各自配对。
        let events = vec![
            started(),
            ModelEvent::ReasoningStarted {
                id: part_id("reasoning_1"),
            },
            ModelEvent::ReasoningDelta {
                id: part_id("reasoning_1"),
                delta: "thinking".to_owned(),
            },
            ModelEvent::ReasoningFinished {
                id: part_id("reasoning_1"),
            },
            ModelEvent::ToolCallStarted {
                id: call_id("call_1"),
                name: agent_types::ToolName::new("get_date").expect("valid tool name"),
            },
            ModelEvent::ToolCallStarted {
                id: call_id("call_2"),
                name: agent_types::ToolName::new("get_time").expect("valid tool name"),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_1"),
                arguments_delta: "{".to_owned(),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_2"),
                arguments_delta: "{".to_owned(),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_1"),
                arguments_delta: "}".to_owned(),
            },
            ModelEvent::ToolCallFinished {
                id: call_id("call_1"),
                arguments: serde_json::json!({}),
            },
            ModelEvent::ToolCallDelta {
                id: call_id("call_2"),
                arguments_delta: "}".to_owned(),
            },
            ModelEvent::ToolCallFinished {
                id: call_id("call_2"),
                arguments: serde_json::json!({}),
            },
            finished(vec![]),
        ];
        let expected_len = events.len();
        let output = validate(events);
        assert_eq!(output.len(), expected_len);
        assert!(matches!(
            output.last(),
            Some(ModelEvent::TurnFinished { .. })
        ));
    }

    #[test]
    fn rejects_delta_without_start() {
        let output = validate(vec![
            started(),
            ModelEvent::TextDelta {
                id: part_id("text_1"),
                delta: "orphan".to_owned(),
            },
            // 违规之后的所有事件都必须被拒绝。
            ModelEvent::TextStarted {
                id: part_id("text_2"),
            },
            finished(vec![]),
        ]);
        assert_eq!(output.len(), 2);
        assert!(matches!(output[0], ModelEvent::TurnStarted { .. }));
        let ModelEvent::TurnFailed {
            error: ModelError::Protocol(message),
        } = &output[1]
        else {
            panic!("expected protocol failure, got {output:?}");
        };
        assert!(message.contains("delta without an open matching part"));
    }

    #[test]
    fn rejects_repeated_finish() {
        let output = validate(vec![
            started(),
            ModelEvent::TextStarted {
                id: part_id("text_1"),
            },
            ModelEvent::TextFinished {
                id: part_id("text_1"),
            },
            ModelEvent::TextFinished {
                id: part_id("text_1"),
            },
        ]);
        assert_eq!(output.len(), 4);
        let ModelEvent::TurnFailed {
            error: ModelError::Protocol(message),
        } = &output[3]
        else {
            panic!("expected protocol failure, got {output:?}");
        };
        assert!(message.contains("finish without an open matching part"));
    }

    #[test]
    fn rejects_events_before_turn_start() {
        let output = validate(vec![
            ModelEvent::UsageUpdated {
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 0,
                    total_tokens: 1,
                    cached_input_tokens: None,
                    reasoning_tokens: None,
                },
            },
            started(),
        ]);
        let message = expect_single_protocol_failure(&output);
        assert!(message.contains("before turn start"));
    }

    #[test]
    fn rejects_duplicate_turn_start() {
        let output = validate(vec![started(), started(), finished(vec![])]);
        assert_eq!(output.len(), 2);
        let ModelEvent::TurnFailed {
            error: ModelError::Protocol(message),
        } = &output[1]
        else {
            panic!("expected protocol failure, got {output:?}");
        };
        assert!(message.contains("duplicate turn start"));
    }

    #[test]
    fn rejects_turn_finish_with_open_parts() {
        let output = validate(vec![
            started(),
            ModelEvent::TextStarted {
                id: part_id("text_1"),
            },
            finished(vec![]),
        ]);
        assert_eq!(output.len(), 3);
        let ModelEvent::TurnFailed {
            error: ModelError::Protocol(message),
        } = &output[2]
        else {
            panic!("expected protocol failure, got {output:?}");
        };
        assert!(message.contains("still open"));
    }

    #[test]
    fn rejects_turn_finish_with_mismatched_message_id() {
        let mismatched = ModelEvent::TurnFinished {
            message: AssistantMessage {
                id: MessageId::new("message_2").expect("valid message id"),
                model: ModelIdentity::new(
                    ProviderId::new("deepseek").expect("valid provider id"),
                    "deepseek-reasoner",
                ),
                parts: vec![],
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        };
        let output = validate(vec![started(), mismatched]);
        let ModelEvent::TurnFailed {
            error: ModelError::Protocol(message),
        } = &output[1]
        else {
            panic!("expected protocol failure, got {output:?}");
        };
        assert!(message.contains("message id different from turn start"));
    }

    #[test]
    fn appends_a_terminal_when_the_stream_ends_without_one() {
        let output = validate(vec![
            started(),
            ModelEvent::TextStarted {
                id: part_id("text_1"),
            },
        ]);
        assert_eq!(output.len(), 3);
        let ModelEvent::TurnFailed {
            error: ModelError::Protocol(message),
        } = &output[2]
        else {
            panic!("expected protocol failure, got {output:?}");
        };
        assert!(message.contains("without a terminal event"));
    }

    #[test]
    fn drops_everything_after_a_terminal() {
        let output = validate(vec![
            started(),
            finished(vec![]),
            ModelEvent::TextStarted {
                id: part_id("text_1"),
            },
            finished(vec![]),
        ]);
        assert_eq!(output.len(), 2);
        assert!(matches!(
            output.last(),
            Some(ModelEvent::TurnFinished { .. })
        ));
    }

    #[test]
    fn keeps_only_the_first_of_two_terminals() {
        let output = validate(vec![
            started(),
            ModelEvent::TurnFailed {
                error: ModelError::Transport("connection reset".to_owned()),
            },
            finished(vec![]),
        ]);
        assert_eq!(output.len(), 2);
        assert_eq!(
            output[1],
            ModelEvent::TurnFailed {
                error: ModelError::Transport("connection reset".to_owned())
            }
        );
    }

    #[test]
    fn turn_failed_is_a_valid_first_event() {
        let output = validate(vec![ModelEvent::TurnFailed {
            error: ModelError::Provider {
                message: "bad request".to_owned(),
                status: Some(400),
            },
        }]);
        assert_eq!(output.len(), 1);
        assert!(matches!(output[0], ModelEvent::TurnFailed { .. }));
    }
}
