//! 模型事件收集与断言：生命周期、唯一终态和最终 AssistantMessage。

use agent_model::{LifecycleValidator, ModelError, ModelEvent, ModelEventStream};
use agent_types::AssistantMessage;
use futures_util::StreamExt;

/// 事件收集入口。全部是关联函数，不产生运行时状态。
pub struct EventCollector;

impl EventCollector {
    /// 原样收集整个事件流。
    pub async fn collect(stream: ModelEventStream) -> CollectedEvents {
        CollectedEvents {
            events: stream.collect().await,
        }
    }

    /// 先经过 [`LifecycleValidator`] 强制契约，再收集。
    pub async fn collect_validated(stream: ModelEventStream) -> CollectedEvents {
        Self::collect(Box::pin(LifecycleValidator::new(stream))).await
    }
}

/// 一次模型调用收集到的事件序列及常用断言。
pub struct CollectedEvents {
    events: Vec<ModelEvent>,
}

impl CollectedEvents {
    /// 全部事件（含终态）。
    pub fn events(&self) -> &[ModelEvent] {
        &self.events
    }

    /// 迭代全部终态事件（`TurnFinished` / `TurnFailed`）。
    pub fn terminals(&self) -> impl Iterator<Item = &ModelEvent> {
        self.events.iter().filter(|event| event.is_terminal())
    }

    /// 断言恰好一个终态并返回它。
    pub fn assert_single_terminal(&self) -> &ModelEvent {
        let terminals: Vec<&ModelEvent> = self.terminals().collect();
        assert_eq!(
            terminals.len(),
            1,
            "expected exactly one terminal event, got {} in {:?}",
            terminals.len(),
            self.events
        );
        terminals[0]
    }

    /// 正常终态时返回最终的 AssistantMessage。
    pub fn finished_message(&self) -> Option<&AssistantMessage> {
        self.events.iter().rev().find_map(|event| match event {
            ModelEvent::TurnFinished { message } => Some(message),
            _ => None,
        })
    }

    /// 异常终态时返回失败原因。
    pub fn failure(&self) -> Option<&ModelError> {
        self.events.iter().rev().find_map(|event| match event {
            ModelEvent::TurnFailed { error } => Some(error),
            _ => None,
        })
    }

    /// 断言以 `TurnFinished` 结束并返回最终消息。
    pub fn assert_finished(&self) -> &AssistantMessage {
        self.finished_message()
            .unwrap_or_else(|| panic!("expected TurnFinished, got {:?}", self.events))
    }

    /// 断言以 `TurnFailed` 结束并返回失败原因。
    pub fn assert_failed(&self) -> &ModelError {
        self.failure()
            .unwrap_or_else(|| panic!("expected TurnFailed, got {:?}", self.events))
    }
}
