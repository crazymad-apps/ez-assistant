//! Runtime 关闭编排：停止准入、取消任务、有界结算并释放 Store。

use std::{collections::BTreeSet, sync::Arc};

use assistant_protocol::{
    ApprovalId, ChildTaskId, GoalId, InputId, RunId, RuntimeEvent, RuntimeLifecycle, SessionId,
    ShutdownRuntimeRequest, ShutdownRuntimeResult, StopGoalRequest,
};
use tokio::time::timeout;

use super::AssistantRuntime;
use crate::{
    RuntimeError, RuntimeResult,
    run::{finished_event, settle_run},
    session::SessionController,
};

/// Runtime 工作快照仅用于判断关闭影响，任务事实继续由各领域 owner 持有。
#[derive(Default)]
pub struct ShutdownWork {
    tasks: BTreeSet<ShutdownTask>,
    approvals: BTreeSet<ApprovalId>,
    queued: BTreeSet<InputId>,
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
enum ShutdownTask {
    Run(RunId, u32),
    Child(ChildTaskId),
    Goal(GoalId, u64),
    Command(InputId),
    Compaction(SessionId, String),
    Title(SessionId, String),
    AppCall(String, String),
    BackgroundShell(String),
    ScheduledExecution(assistant_protocol::ScheduleExecutionId),
}

impl ShutdownWork {
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.tasks.len(), self.approvals.len(), self.queued.len())
    }

    fn covered_by(&self, confirmed: &Self) -> bool {
        self.tasks.is_subset(&confirmed.tasks)
            && self.approvals.is_subset(&confirmed.approvals)
            && self.queued.is_subset(&confirmed.queued)
    }
}

impl AssistantRuntime {
    pub async fn preview_shutdown(&self) -> RuntimeResult<ShutdownWork> {
        let _operation = self.operation_gate.write().await;
        self.ensure_running()?;
        let sessions = self.shutdown_sessions()?;
        let mut mutations = Vec::with_capacity(sessions.len());
        for session in &sessions {
            mutations.push(session.mutation().await);
        }
        self.shutdown_work(&sessions).await
    }

    /// 在同一个接纳边界比较实际工作并关闭入口。只完成的工作允许从快照中移除。
    pub async fn begin_confirmed_shutdown(&self, confirmed: &ShutdownWork) -> RuntimeResult<bool> {
        let _operation = self.operation_gate.write().await;
        self.ensure_running()?;
        let sessions = self.shutdown_sessions()?;
        let mut mutations = Vec::with_capacity(sessions.len());
        for session in &sessions {
            mutations.push(session.mutation().await);
        }
        if !self.shutdown_work(&sessions).await?.covered_by(confirmed)
            || !self
                .approval_registry
                .close_if_covered(&confirmed.approvals)?
        {
            return Ok(false);
        }
        self.begin_shutdown()?;
        // 在释放队列领取的 Session 门禁前，阻止正在准备的输入和后续队列项启动。
        self.root_cancellation.cancel();
        Ok(true)
    }

    /// 与普通进程关闭共用资源关闭路径。超时、未结算或故障状态均不能作为交接成功。
    pub async fn finish_confirmed_shutdown(&self) -> RuntimeResult<()> {
        let _operation = self.operation_gate.write().await;
        let lifecycle = self.lifecycle()?;
        if lifecycle != RuntimeLifecycle::ShuttingDown {
            return Err(RuntimeError::RuntimeNotRunning { lifecycle });
        }
        let sessions = self.shutdown_sessions()?;
        for cancellation in self
            .request_active_run_cancellation(sessions.clone())
            .await?
        {
            cancellation.cancel();
        }
        if !self.tasks.wait(self.config.shutdown_timeout).await {
            self.confirm_background_cleanup()?;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "runtime work shutdown timed out",
            });
        }
        self.confirm_background_cleanup()?;
        for session in &sessions {
            let state = session.lock_state()?;
            if state.is_faulted
                || state.tool_cleanup_unconfirmed
                || state.active_run.is_some()
                || state.executing_command.is_some()
                || state.active_compaction.is_some()
                || state.active_title_generation.is_some()
                || state
                    .journal
                    .as_ref()
                    .is_some_and(crate::journal::InMemoryJournal::has_pending)
                || self.child_tasks.active_count_for_session(session.id())? != 0
                || !self.approval_registry.list(session.id())?.is_empty()
            {
                return Err(RuntimeError::InternalStateUnavailable {
                    component: "runtime work settlement",
                });
            }
        }
        for session in &sessions {
            let goal = session.lock_state()?.goal.clone();
            if let Some(goal) =
                goal.filter(|goal| matches!(goal.state, crate::goal::GoalState::Running))
            {
                self.stop_goal_in_session(
                    StopGoalRequest {
                        session_id: session.id().clone(),
                        goal_id: goal.id,
                        expected_generation: goal.generation,
                    },
                    session,
                )
                .await?;
            }
        }
        self.shutdown_mcp_with_timeout().await?;
        self.shutdown_store_with_timeout().await?;
        self.set_stopped()
    }

    async fn shutdown_work(
        &self,
        sessions: &[Arc<SessionController>],
    ) -> RuntimeResult<ShutdownWork> {
        let mut work = ShutdownWork {
            queued: self
                .store
                .queued_input_ids()
                .await
                .map_err(|source| {
                    RuntimeError::from_store("read queued work before shutdown", source)
                })?
                .into_iter()
                .collect(),
            ..ShutdownWork::default()
        };
        for session in sessions {
            let state = session.lock_state()?;
            if let Some(run) = &state.active_run {
                let snapshot = state
                    .runs
                    .get(&run.run_id)
                    .ok_or(RuntimeError::InternalStateUnavailable {
                        component: "active run record",
                    })?
                    .snapshot();
                work.queued.remove(&snapshot.input_id);
                work.tasks
                    .insert(ShutdownTask::Run(run.run_id.clone(), snapshot.attempt));
            }
            if let Some(goal) = &state.goal
                && matches!(goal.state, crate::goal::GoalState::Running)
            {
                work.tasks
                    .insert(ShutdownTask::Goal(goal.id.clone(), goal.generation));
            }
            if let Some(command) = &state.executing_command {
                work.queued.remove(command);
                work.tasks.insert(ShutdownTask::Command(command.clone()));
            }
            if let Some(compaction) = &state.active_compaction {
                work.tasks.insert(ShutdownTask::Compaction(
                    session.id().clone(),
                    compaction.snapshot.compaction_id.clone(),
                ));
            }
            if let Some(title) = &state.active_title_generation {
                work.tasks.insert(ShutdownTask::Title(
                    session.id().clone(),
                    title.task_id.clone(),
                ));
            }
            work.tasks.extend(
                self.child_tasks
                    .list_for_session(session.id())?
                    .into_iter()
                    .filter(|child| !child.status.is_terminal())
                    .map(|child| ShutdownTask::Child(child.child_task_id)),
            );
            work.approvals.extend(
                self.approval_registry
                    .list(session.id())?
                    .into_iter()
                    .map(|approval| approval.approval_id),
            );
        }
        work.tasks.extend(
            self.ui_tools
                .active_ids()?
                .into_iter()
                .map(|(instance, request)| ShutdownTask::AppCall(instance, request)),
        );
        work.tasks.extend(
            self.background_runs
                .ids()
                .into_iter()
                .map(ShutdownTask::BackgroundShell),
        );
        work.tasks.extend(
            self.scheduler
                .active_ids()
                .into_iter()
                .map(ShutdownTask::ScheduledExecution),
        );
        work.approvals
            .extend(self.approval_registry.app_approval_ids()?);
        Ok(work)
    }

    /// 与正在准备/接纳的业务操作串行后再检查任务，避免客户端到期时抢先关闭尚在接纳的工作。
    pub async fn request_shutdown_if_unused(
        &self,
        still_unused: impl FnOnce() -> bool,
    ) -> RuntimeResult<bool> {
        let _operation = self.operation_gate.write().await;
        if self.has_background_work() || !still_unused() {
            return Ok(false);
        }
        self.request_shutdown()?;
        Ok(true)
    }

    /// 宿主结束用户访问时立即封闭准入并通知执行取消；资源仍须由 shutdown 异步收尾。
    pub fn request_shutdown(&self) -> RuntimeResult<()> {
        self.begin_shutdown()?;
        self.root_cancellation.cancel();
        Ok(())
    }

    /// 拒绝新工作、取消活动 Run，并在各自等待上限内收敛 supervisor 与 Store。
    pub async fn shutdown(
        &self,
        _request: ShutdownRuntimeRequest,
    ) -> RuntimeResult<ShutdownRuntimeResult> {
        let _operation = self.operation_gate.write().await;
        if self.lifecycle()? == RuntimeLifecycle::Stopped {
            self.confirm_background_cleanup()?;
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

        // Run 已停止或完成强制结算后才 retire MCP；这样已取得的调用 lease 有机会先归零，
        // Registry 关闭也不会与 Store shutdown 争抢工具可靠事实。
        let mcp_result = self.shutdown_mcp_with_timeout().await;

        // 即使强制结算失败，也必须尝试 flush/join Store；非终态 Run 会在下次启动时
        // 按持久事实恢复，不能因为前一步错误把 worker 永久留给当前 Runtime。
        let store_result = self.shutdown_store_with_timeout().await;
        if store_result.is_ok() {
            self.set_stopped()?;
        }
        store_result?;
        self.confirm_background_cleanup()?;
        mcp_result?;
        settlement_result?;
        Ok(ShutdownRuntimeResult {
            lifecycle: RuntimeLifecycle::Stopped,
        })
    }

    fn confirm_background_cleanup(&self) -> RuntimeResult<()> {
        if self.scheduler.has_unconfirmed_cleanup() {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "scheduled script cleanup or settlement unconfirmed",
            });
        }
        if self.background_runs.needs_cleanup() {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "background shell process cleanup unconfirmed",
            });
        }
        Ok(())
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
        self.shutdown_sessions()
    }

    fn shutdown_sessions(&self) -> RuntimeResult<Vec<Arc<SessionController>>> {
        self.sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })
            .map(|sessions| sessions.values().cloned().collect())
    }

    async fn shutdown_mcp_with_timeout(&self) -> RuntimeResult<()> {
        timeout(
            self.config.shutdown_timeout,
            self.mcp_service.registry.shutdown(),
        )
        .await
        .unwrap_or(Err(RuntimeError::InternalStateUnavailable {
            component: "MCP shutdown timed out",
        }))
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
                let controller = self.controller_tool_coordinator();
                let settlement = settle_run(
                    &session,
                    &run_id,
                    None,
                    crate::run::RunSettlementContext::new(
                        self.store.as_ref(),
                        controller.as_ref(),
                        self.output_dispatcher.as_ref(),
                        false,
                    ),
                    None,
                )
                .await?;
                if let Some(goal) = settlement.goal {
                    self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
                        session_id: session.id().clone(),
                        goal_id: goal.goal_id,
                        generation: goal.generation,
                    });
                }
                self.publish(finished_event(settlement.run, false));
                let generation = session.lock_state()?.body_generation;
                self.publish(assistant_protocol::RuntimeEvent::ConversationCommitted {
                    owner: assistant_protocol::ConversationOwner::MainSession {
                        session_id: session.id().clone(),
                    },
                    generation,
                });
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
