//! 可控取消 gate：在指定事件序号处精确触发取消，不依赖 sleep。

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use agent_model::{ModelEvent, ModelEventStream};
use futures_util::Stream;
use tokio_util::sync::CancellationToken;

/// 连接测试与被测服务的取消闸门。
///
/// 用法：把 [`token`](Self::token) 交给 `ModelCallContext`，再用
/// [`watch`](Self::watch) 包装返回的事件流。闸门每经过一个事件计数一次，
/// 计数达到 [`cancel_after`](Self::cancel_after) 预设值时取消 token；
/// 被取消的服务按契约以唯一 `TurnFailed(Cancelled)` 受控结束。
#[derive(Clone)]
pub struct CancelGate {
    token: CancellationToken,
    state: Arc<Mutex<GateState>>,
}

#[derive(Debug, Default)]
struct GateState {
    armed_after: Option<u64>,
    emitted: u64,
    fired: bool,
}

impl CancelGate {
    /// 创建尚未触发的闸门。
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            state: Arc::new(Mutex::new(GateState::default())),
        }
    }

    /// 交给 `ModelCallContext` 的取消令牌。
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// 武装：第 `count` 个事件通过后立即取消。
    ///
    /// `count` 必须大于 0；建立前取消请直接用 [`cancel_now`](Self::cancel_now)。
    pub fn cancel_after(&self, count: u64) {
        assert!(
            count > 0,
            "cancel_after requires count >= 1; use cancel_now for pre-stream cancellation"
        );
        self.state
            .lock()
            .expect("cancel gate mutex poisoned")
            .armed_after = Some(count);
    }

    /// 立即取消。
    pub fn cancel_now(&self) {
        let mut state = self.state.lock().expect("cancel gate mutex poisoned");
        if !state.fired {
            state.fired = true;
            self.token.cancel();
        }
    }

    /// 已经过的事件数。
    pub fn emitted(&self) -> u64 {
        self.state
            .lock()
            .expect("cancel gate mutex poisoned")
            .emitted
    }

    /// 闸门是否已经触发。
    pub fn fired(&self) -> bool {
        self.state.lock().expect("cancel gate mutex poisoned").fired
    }

    /// 包装任意事件流：事件原样通过，每通过一个计数一次。
    pub fn watch(&self, stream: ModelEventStream) -> ModelEventStream {
        Box::pin(GatedStream {
            inner: stream,
            gate: self.clone(),
        })
    }

    fn on_event(&self) {
        let mut state = self.state.lock().expect("cancel gate mutex poisoned");
        state.emitted += 1;
        let armed = state
            .armed_after
            .is_some_and(|count| state.emitted >= count);
        if armed && !state.fired {
            state.fired = true;
            self.token.cancel();
        }
    }
}

impl Default for CancelGate {
    fn default() -> Self {
        Self::new()
    }
}

/// 透传事件并为 gate 计数的包装流。
struct GatedStream {
    inner: ModelEventStream,
    gate: CancelGate,
}

impl Stream for GatedStream {
    type Item = ModelEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ModelEvent>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(event)) => {
                this.gate.on_event();
                Poll::Ready(Some(event))
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelError, ModelService, SystemPromptSnapshot,
    };
    use agent_types::{
        AssistantMessage, FinishReason, MessageId, ModelIdentity, PartId, ProviderId, TextPart,
    };

    use super::*;
    use crate::{EventCollector, ScriptedModelService};

    fn service() -> ScriptedModelService {
        let message = AssistantMessage {
            id: MessageId::new("message_1").expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("deepseek").expect("valid provider id"),
                "deepseek-reasoner",
            ),
            parts: vec![agent_types::AssistantPart::Text(TextPart {
                id: PartId::new("text_1").expect("valid part id"),
                text: "hello".to_owned(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        };
        ScriptedModelService::completing(
            ModelCapabilities {
                reasoning: false,
                image_input: false,
                tool_calls: false,
                streaming: true,
            },
            128_000,
            message,
        )
    }

    fn request() -> agent_model::ModelRequest {
        use agent_types::{ConversationSnapshot, ToolChoice};
        agent_model::ModelRequest {
            system: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: agent_model::GenerationConfig::default(),
            reasoning: None,
            provider_options: agent_model::ProviderOptions::new(),
        }
    }

    #[tokio::test]
    async fn cancel_after_exact_event_count() {
        let gate = CancelGate::new();
        // TurnStarted + TextStarted 两个事件后取消。
        gate.cancel_after(2);
        let service = service();
        let context = ModelCallContext::new(gate.token());
        let stream = service
            .stream(request(), context)
            .await
            .expect("stream established");
        let collected = EventCollector::collect(gate.watch(stream)).await;

        assert!(gate.fired());
        assert_eq!(gate.emitted(), 3); // 2 个正常事件 + 1 个受控 TurnFailed
        assert!(matches!(
            collected.events()[0],
            ModelEvent::TurnStarted { .. }
        ));
        assert!(matches!(
            collected.events()[1],
            ModelEvent::TextStarted { .. }
        ));
        assert_eq!(collected.assert_failed(), &ModelError::Cancelled);
        collected.assert_single_terminal();
    }

    #[tokio::test]
    async fn cancel_now_cancels_before_establishment() {
        let gate = CancelGate::new();
        gate.cancel_now();
        let service = service();
        let context = ModelCallContext::new(gate.token());
        let error = service
            .stream(request(), context)
            .await
            .err()
            .expect("cancelled before establishment");
        assert_eq!(error, ModelError::Cancelled);
    }
}
