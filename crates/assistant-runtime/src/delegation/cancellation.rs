//! 单个活动子任务的取消令牌与唯一原因。

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use assistant_protocol::{ChildTaskId, RunId, SessionId};
use tokio_util::sync::CancellationToken;

use super::ChildTaskRegistry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ChildCancellationReason {
    Requested = 1,
    Timeout = 2,
}

/// 原因必须先于 token 发布，终态结算才能区分显式取消和超时。
pub(crate) struct ChildTaskCancellation {
    token: CancellationToken,
    reason: AtomicU8,
}

impl ChildTaskCancellation {
    pub(super) fn child_of(parent: &CancellationToken) -> Self {
        Self {
            token: parent.child_token(),
            reason: AtomicU8::new(0),
        }
    }

    pub(super) fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub(super) fn request(&self, reason: ChildCancellationReason) {
        let _ = self
            .reason
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire);
        self.token.cancel();
    }

    pub(super) fn reason(&self) -> Option<ChildCancellationReason> {
        match self.reason.load(Ordering::Acquire) {
            value if value == ChildCancellationReason::Requested as u8 => {
                Some(ChildCancellationReason::Requested)
            }
            value if value == ChildCancellationReason::Timeout as u8 => {
                Some(ChildCancellationReason::Timeout)
            }
            _ => None,
        }
    }
}

/// 活动索引只覆盖实际 delegate future；任何提前返回都会同步移除控制句柄。
pub(super) struct ActiveChildGuard {
    registry: Arc<ChildTaskRegistry>,
    child_task_id: ChildTaskId,
}

impl ActiveChildGuard {
    pub(super) fn new(registry: Arc<ChildTaskRegistry>, child_task_id: ChildTaskId) -> Self {
        Self {
            registry,
            child_task_id,
        }
    }
}

impl Drop for ActiveChildGuard {
    fn drop(&mut self) {
        let _ = self.registry.deactivate(&self.child_task_id);
    }
}

/// permit 获取后的工作阶段计时器；Drop 会停止尚未触发的后台计时任务。
pub(super) struct ChildTimeoutGuard {
    task: tokio::task::JoinHandle<()>,
}

impl ChildTimeoutGuard {
    pub(super) fn start(
        duration: std::time::Duration,
        parent_cancellation: CancellationToken,
        registry: Arc<ChildTaskRegistry>,
        session_id: SessionId,
        parent_run_id: RunId,
        child_task_id: ChildTaskId,
    ) -> Self {
        Self {
            task: tokio::spawn(async move {
                let reason = tokio::select! {
                    biased;
                    () = parent_cancellation.cancelled() => ChildCancellationReason::Requested,
                    () = tokio::time::sleep(duration) => ChildCancellationReason::Timeout,
                };
                let _ = registry.cancel_active(&session_id, &parent_run_id, &child_task_id, reason);
            }),
        }
    }
}

impl Drop for ChildTimeoutGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}
