//! Runtime 产品观察事件的单一序号分配与广播边界。

use std::{
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{RuntimeEvent, RuntimeEventEnvelope};
use tokio::sync::broadcast;

struct ObservationState {
    sequence: u64,
}

struct ObservationInner {
    state: Mutex<ObservationState>,
    sender: broadcast::Sender<RuntimeEventEnvelope>,
    legacy_sender: broadcast::Sender<RuntimeEvent>,
}

/// 所有 Runtime 可观察事件共享的短发布边界。
///
/// 序号分配和广播在同一个非异步临界区内完成，因此并发发布者在接收端看到的顺序与
/// `sequence` 完全一致。这里不持有业务状态，也不会跨 Store I/O、Provider 调用或审批等待。
#[derive(Clone)]
pub(crate) struct ObservationCoordinator {
    inner: Arc<ObservationInner>,
}

impl ObservationCoordinator {
    pub(crate) fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        // 裸事件流只服务既有嵌入式调用。聚合失效事件增加后给它保留兼容余量；正式 Host
        // 仍使用上面的精确产品容量，并在落后时通过 stream_gap 恢复。
        let legacy_capacity = capacity.saturating_mul(4).max(capacity);
        let (legacy_sender, _) = broadcast::channel(legacy_capacity);
        Self {
            inner: Arc::new(ObservationInner {
                state: Mutex::new(ObservationState { sequence: 0 }),
                sender,
                legacy_sender,
            }),
        }
    }

    /// 在当前 Runtime 实例内分配严格递增序号并发布事件。
    pub(crate) fn send(&self, event: RuntimeEvent) -> Result<usize, ()> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.sequence = state.sequence.saturating_add(1);
        let legacy_event = event.clone();
        let envelope = RuntimeEventEnvelope {
            sequence: state.sequence,
            emitted_at_ms: emitted_at_ms(),
            event,
        };
        let result = self.inner.sender.send(envelope).map_err(|_| ());
        // 裸事件订阅只保留给 Runtime crate 的既有嵌入式调用和测试；产品 Host 使用 envelope。
        let _ = self.inner.legacy_sender.send(legacy_event);
        result
    }

    /// 返回已经完成发布的最后一个事件序号。
    pub(crate) fn sequence(&self) -> u64 {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sequence
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeEventEnvelope> {
        self.inner.sender.subscribe()
    }

    pub(crate) fn subscribe_legacy(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.inner.legacy_sender.subscribe()
    }
}

fn emitted_at_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_publish_order_matches_sequence_order() {
        let coordinator = ObservationCoordinator::new(8);
        let mut receiver = coordinator.subscribe();
        let left = coordinator.clone();
        let right = coordinator.clone();

        let (left_result, right_result) = tokio::join!(
            tokio::spawn(async move { left.send(RuntimeEvent::RuntimeShuttingDown) }),
            tokio::spawn(async move { right.send(RuntimeEvent::RuntimeShuttingDown) }),
        );
        left_result.expect("left publisher").expect("left event");
        right_result.expect("right publisher").expect("right event");

        assert_eq!(receiver.recv().await.expect("first").sequence, 1);
        assert_eq!(receiver.recv().await.expect("second").sequence, 2);
        assert_eq!(coordinator.sequence(), 2);
    }
}
