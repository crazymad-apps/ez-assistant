//! stdio 取消路径的短期资源回收。Factory 持有本组并在 Runtime 退出后显式等待。

use futures_util::FutureExt as _;
use process_wrap::tokio::ChildWrapper;
use std::{
    future::Future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::task::{TaskTracker, task_tracker::TaskTrackerToken};

#[derive(Default)]
pub(super) struct McpProcessCleanup {
    tasks: TaskTracker,
    failed: Arc<AtomicBool>,
}

impl McpProcessCleanup {
    pub(super) fn lease(&self) -> TaskTrackerToken {
        self.tasks.token()
    }

    pub(super) fn enqueue(
        &self,
        mut child: Option<Box<dyn ChildWrapper>>,
        stderr: Option<JoinHandle<()>>,
    ) {
        if let Some(task) = &stderr {
            task.abort();
        }
        if child.is_none() && stderr.is_none() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            if let Some(child) = &mut child {
                let _ = child.start_kill();
            }
            self.failed.store(true, Ordering::Release);
            eprintln!("runtime-host: MCP cleanup requested after executor shutdown");
            return;
        }
        self.track(async move {
            let process_result = if let Some(mut child) = child {
                let _ = child.start_kill();
                child.wait().await.map(|_| ())
            } else {
                Ok(())
            };
            if let Some(task) = stderr
                && let Err(error) = task.await
                && !error.is_cancelled()
            {
                return Err(io::Error::other("MCP diagnostic task failed"));
            }
            process_result
        });
    }

    fn track(&self, cleanup: impl Future<Output = io::Result<()>> + Send + 'static) {
        let failed = self.failed.clone();
        // JoinHandle 不用于结果传递：Tracker 跟踪完成，任务捕获错误/panic/超时，shutdown
        // 消费聚合失败标记。这样既不会静默 detach，也不会把潜在 secret 的 panic payload 上报。
        self.tasks.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                std::panic::AssertUnwindSafe(cleanup).catch_unwind(),
            )
            .await;
            if !matches!(result, Ok(Ok(Ok(())))) {
                failed.store(true, Ordering::Release);
                eprintln!("runtime-host: MCP resource cleanup failed");
            }
        });
    }

    pub(super) async fn shutdown(&self) -> io::Result<()> {
        self.tasks.close();
        tokio::time::timeout(Duration::from_secs(4), self.tasks.wait())
            .await
            .map_err(|_| io::Error::other("MCP cleanup tasks did not finish"))?;
        if self.failed.load(Ordering::Acquire) {
            return Err(io::Error::other("MCP resource cleanup failed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_waits_for_owned_cleanup_and_reports_failure() {
        let cleanup = Arc::new(McpProcessCleanup::default());
        let lease = cleanup.lease();
        let closing = cleanup.shutdown();
        tokio::pin!(closing);
        assert!(futures_util::poll!(&mut closing).is_pending());
        let (release, wait) = tokio::sync::oneshot::channel();
        cleanup.track(async move {
            wait.await.expect("release cleanup");
            Err(io::Error::other("fixture failure"))
        });
        drop(lease);
        assert!(futures_util::poll!(&mut closing).is_pending());
        release.send(()).expect("release");
        assert!(closing.await.is_err());
        assert!(cleanup.tasks.is_empty());
    }
}
