//! Runtime 所有的后台任务登记、等待与超时中止。

use std::{
    collections::BTreeMap,
    future::Future,
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{
    FutureExt,
    future::{AbortHandle, Abortable},
};
use tokio::time::timeout;
use tokio_util::task::TaskTracker;

/// 只跟踪 Runtime 自己启动的 supervisor；Core 内部任务仍由 Core 契约所有。
pub(super) struct RuntimeTasks {
    tracker: TaskTracker,
    next_id: AtomicU64,
    aborts: Arc<Mutex<BTreeMap<u64, AbortHandle>>>,
}

impl RuntimeTasks {
    pub(super) fn new() -> Self {
        Self {
            tracker: TaskTracker::new(),
            next_id: AtomicU64::new(0),
            aborts: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// 先登记中止句柄再启动任务，避免短任务先结束、后留下过期句柄。
    pub(super) fn spawn(
        &self,
        future: impl Future<Output = ()> + Send + 'static,
        on_panic: impl Future<Output = ()> + Send + 'static,
    ) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (abort, registration) = AbortHandle::new_pair();
        self.abort_handles().insert(id, abort);
        let aborts = self.aborts.clone();
        self.tracker.spawn(async move {
            let outcome = AssertUnwindSafe(Abortable::new(future, registration))
                .catch_unwind()
                .await;
            // Abort 是受控关闭；只有真正的 panic 才进入调用方提供的领域收敛路径。
            if outcome.is_err() {
                // 恢复逻辑也是受管任务的一部分。它再次 panic 时不能跳过登记清理，
                // 具体领域 owner 仍负责在自己的恢复 future 内建立 fail-closed 状态。
                let _ = AssertUnwindSafe(on_panic).catch_unwind().await;
            }
            aborts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
        });
    }

    pub(super) fn close(&self) {
        self.tracker.close();
    }

    /// 返回 `true` 表示全部任务优雅退出；`false` 表示超时后已中止剩余 supervisor。
    pub(super) async fn wait_or_abort(&self, maximum_wait: Duration) -> bool {
        if timeout(maximum_wait, self.tracker.wait()).await.is_ok() {
            return true;
        }

        let aborts = std::mem::take(&mut *self.abort_handles());
        for abort in aborts.into_values() {
            abort.abort();
        }
        // Abortable 包装层会在 abort 后收敛；此处等待不再依赖业务 future 配合取消。
        self.tracker.wait().await;
        false
    }

    fn abort_handles(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, AbortHandle>> {
        self.aborts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    #[tokio::test]
    async fn task_panic_runs_owner_recovery_before_tracker_finishes() {
        let tasks = RuntimeTasks::new();
        let observed = Arc::new(AtomicBool::new(false));
        let recovery_observed = observed.clone();
        tasks.spawn(
            async { panic!("private runtime task panic payload") },
            async move { recovery_observed.store(true, Ordering::Release) },
        );
        tasks.close();

        assert!(tasks.wait_or_abort(Duration::from_secs(1)).await);
        assert!(observed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn recovery_panic_is_contained_and_task_registration_is_cleared() {
        let tasks = RuntimeTasks::new();
        tasks.spawn(
            async { panic!("private runtime task panic payload") },
            async { panic!("private recovery panic payload") },
        );
        tasks.close();

        assert!(tasks.wait_or_abort(Duration::from_secs(1)).await);
        assert!(tasks.abort_handles().is_empty());
    }
}
