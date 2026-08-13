//! Runtime 关闭编排：停止准入、取消任务、有界结算并释放 Store。

use std::sync::Arc;

use assistant_protocol::{
    RuntimeEvent, RuntimeLifecycle, ShutdownRuntimeRequest, ShutdownRuntimeResult,
};
use tokio::time::timeout;

use super::AssistantRuntime;
use crate::{
    RuntimeError, RuntimeResult,
    run::{finished_event, settle_run},
    session::SessionController,
};

impl AssistantRuntime {
    /// 拒绝新工作、取消活动 Run，并在各自等待上限内收敛 supervisor 与 Store。
    pub async fn shutdown(
        &self,
        _request: ShutdownRuntimeRequest,
    ) -> RuntimeResult<ShutdownRuntimeResult> {
        let _operation = self.operation_gate.write().await;
        if self.lifecycle()? == RuntimeLifecycle::Stopped {
            return Ok(ShutdownRuntimeResult {
                lifecycle: RuntimeLifecycle::Stopped,
            });
        }
        let sessions = self.begin_shutdown()?;

        let cancellations = self.request_active_run_cancellation(sessions).await?;
        for cancellation in cancellations {
            cancellation.cancel();
        }
        self.root_cancellation.cancel();

        let graceful = self.tasks.wait_or_abort(self.config.shutdown_timeout).await;
        let settlement_result = if graceful {
            Ok(())
        } else {
            self.force_settle_with_timeout().await
        };

        // 即使强制结算失败，也必须尝试 flush/join Store；非终态 Run 会在下次启动时
        // 按持久事实恢复，不能因为前一步错误把 worker 永久留给当前 Runtime。
        let store_result = self.shutdown_store_with_timeout().await;
        if store_result.is_ok() {
            self.set_stopped()?;
        }
        store_result?;
        settlement_result?;
        Ok(ShutdownRuntimeResult {
            lifecycle: RuntimeLifecycle::Stopped,
        })
    }

    fn begin_shutdown(&self) -> RuntimeResult<Vec<Arc<SessionController>>> {
        let mut lifecycle =
            self.lifecycle
                .write()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "runtime lifecycle",
                })?;
        if *lifecycle == RuntimeLifecycle::Stopped {
            return Ok(Vec::new());
        }
        let first_transition = *lifecycle == RuntimeLifecycle::Running;
        *lifecycle = RuntimeLifecycle::ShuttingDown;
        self.tasks.close();
        if first_transition {
            self.publish(RuntimeEvent::RuntimeShuttingDown);
        }
        self.sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })
            .map(|sessions| sessions.values().cloned().collect())
    }

    async fn request_active_run_cancellation(
        &self,
        sessions: Vec<Arc<SessionController>>,
    ) -> RuntimeResult<Vec<tokio_util::sync::CancellationToken>> {
        let mut cancellations = Vec::new();
        for session in sessions {
            // 与 supervisor/cancel 的 Store await 边界串行，避免终态提交期间再次改写取消投影。
            let _mutation = session.mutation().await;
            let Some((run_id, cancellation)) = session
                .lock_state()?
                .active_run
                .as_ref()
                .map(|active| (active.run_id.clone(), active.cancellation.clone()))
            else {
                continue;
            };
            self.cancel_run_approvals(session.id(), &run_id).await?;
            let mut state = session.lock_state()?;
            let first_request = state
                .runs
                .get_mut(&run_id)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "active run record",
                })?
                .mark_cancelling();
            if first_request {
                self.publish(RuntimeEvent::RunCancelling {
                    session_id: session.id().clone(),
                    run_id,
                });
            }
            cancellations.push(cancellation);
        }
        Ok(cancellations)
    }

    async fn force_settle_with_timeout(&self) -> RuntimeResult<()> {
        timeout(
            self.config.shutdown_timeout,
            self.force_settle_active_runs(),
        )
        .await
        .map_err(|_| RuntimeError::StorageUnavailable {
            operation: "force-settle active runs before the shutdown timeout",
            source: None,
        })?
    }

    async fn shutdown_store_with_timeout(&self) -> RuntimeResult<()> {
        timeout(self.config.shutdown_timeout, self.store.shutdown())
            .await
            .map_err(|_| RuntimeError::StorageUnavailable {
                operation: "shut down runtime storage before the shutdown timeout",
                source: None,
            })?
            .map_err(|source| RuntimeError::from_store("shut down runtime storage", source))
    }

    /// supervisor 超时被中止后，不将 Cancelling Run 遗留为非终态权威事实。
    async fn force_settle_active_runs(&self) -> RuntimeResult<()> {
        let sessions: Vec<_> = self
            .sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .values()
            .cloned()
            .collect();
        for session in sessions {
            let (run_id, has_pending) = {
                let state = session.lock_state()?;
                (
                    state
                        .active_run
                        .as_ref()
                        .map(|active| active.run_id.clone()),
                    state
                        .journal
                        .as_ref()
                        .is_some_and(crate::journal::InMemoryJournal::has_pending),
                )
            };
            if let Some(run_id) = run_id {
                if has_pending {
                    // 强制关闭不能把副作用结果未知的 pending 伪装成普通 Failed。保留
                    // begun/ready 与非终态 Run，下一次启动先补齐工具结果再中断 Run。
                    let mut state = session.lock_state()?;
                    state.active_run = None;
                    state.is_queue_driver_running = false;
                    state.is_faulted = true;
                    continue;
                }
                let snapshot =
                    settle_run(&session, &run_id, None, self.store.as_ref(), None).await?;
                self.publish(finished_event(snapshot));
            }
        }
        Ok(())
    }

    fn set_stopped(&self) -> RuntimeResult<()> {
        *self
            .lifecycle
            .write()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "runtime lifecycle",
            })? = RuntimeLifecycle::Stopped;
        Ok(())
    }
}
