//! 只为本 Runtime 等待最近一次到期。Store 提交后才登记执行，关机等待同一任务跟踪器。

use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use assistant_protocol::{
    ListBackgroundRunsRequest, ListScheduleExecutionsRequest, ListScheduleExecutionsResult,
    RunScheduleNowRequest, RunScheduleNowResult, ScheduleAction, ScheduleAgentResult,
    ScheduleExecution, ScheduleExecutionId, ScheduleExecutionReason, ScheduleExecutionStatus,
    ScheduleExecutionSummary, ScheduleExecutionTarget, ScheduleId, ScheduleScriptResult,
};
use chrono::{DateTime, Utc};
use tokio::{sync::watch, time::Instant};
use tokio_util::sync::CancellationToken;

use super::next_local;
use crate::{
    AssistantRuntime, RuntimeError, RuntimeResult, ScheduleAdmission, ScheduleAdmitted,
    ScheduleResult, ScheduleSettlement, ScheduleTrigger, StoredWorkspace, id,
};

const WAKE_GRACE_MS: i64 = 5_000;
const CLOCK_RECHECK: Duration = Duration::from_secs(5);
const CLOCK_DRIFT_MS: i64 = 1_000;

pub type ScheduleScriptFuture<'a> =
    Pin<Box<dyn Future<Output = (ScheduleExecutionStatus, ScheduleScriptResult)> + Send + 'a>>;

/// Host 提供具体 Shell、目录和平台限制。端口只接受已可靠接纳的本用户脚本。
pub trait ScheduleScriptExecutor: Send + Sync {
    /// Host 在用户域开放前完成旧进程核验。只有登记过的脚本可依此释放未知结果的名额。
    fn records_process_identity(&self) -> bool {
        false
    }
    fn startup_cleanup_confirmed(&self) -> bool {
        false
    }

    fn execute<'a>(
        &'a self,
        schedule_id: &'a ScheduleId,
        execution_id: &'a ScheduleExecutionId,
        workspace: Option<&'a StoredWorkspace>,
        command: String,
        cancellation: CancellationToken,
    ) -> ScheduleScriptFuture<'a>;
}

pub(in crate::runtime) struct Scheduler {
    executor: OnceLock<Arc<dyn ScheduleScriptExecutor>>,
    pub(super) runtime: OnceLock<Weak<AssistantRuntime>>,
    result_errors: Mutex<BTreeMap<ScheduleExecutionId, String>>,
    changed: watch::Sender<()>,
    waiting: AtomicBool,
    maintenance: AtomicBool,
    cleanup_unconfirmed: AtomicBool,
    running: Mutex<BTreeMap<ScheduleExecutionId, (ScheduleId, CancellationToken)>>,
}

impl Scheduler {
    pub(in crate::runtime) fn new() -> Self {
        Self {
            executor: OnceLock::new(),
            runtime: OnceLock::new(),
            result_errors: Mutex::default(),
            changed: watch::channel(()).0,
            waiting: AtomicBool::new(false),
            maintenance: AtomicBool::new(false),
            cleanup_unconfirmed: AtomicBool::new(false),
            running: Mutex::new(BTreeMap::new()),
        }
    }

    pub(in crate::runtime) fn runtime(&self) -> Option<Weak<AssistantRuntime>> {
        self.runtime.get().cloned()
    }

    pub(super) fn note_result_attempt(&self, id: ScheduleExecutionId, error: Option<String>) {
        let mut errors = self
            .result_errors
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(error) = error {
            if errors.len() >= 128 && !errors.contains_key(&id) {
                errors.pop_first();
            }
            errors.insert(id, error);
        } else {
            errors.remove(&id);
        }
    }

    pub(in crate::runtime) fn has_deadline(&self) -> bool {
        self.waiting.load(Ordering::Acquire) || self.maintenance.load(Ordering::Acquire)
    }

    pub(in crate::runtime) fn is_configured(&self) -> bool {
        self.executor.get().is_some()
    }

    pub(in crate::runtime) fn has_unconfirmed_cleanup(&self) -> bool {
        self.cleanup_unconfirmed.load(Ordering::Acquire) || !self.active_ids().is_empty()
    }

    pub(super) fn cancel_plan(&self, id: &ScheduleId) -> bool {
        let running = self
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut active = false;
        for (_, token) in running.values().filter(|(schedule, _)| schedule == id) {
            token.cancel();
            active = true;
        }
        active
    }

    pub(in crate::runtime) fn active_ids(&self) -> Vec<ScheduleExecutionId> {
        self.running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

impl AssistantRuntime {
    /// Host 在发布当前用户 Runtime 前恢复执行事实并装配唯一调度器。
    /// 先恢复不明执行，再从当前时刻计算未来游标。持有生命周期和计划门禁，提交后才开放运行。
    ///
    /// # Errors
    /// 存储恢复失败或调度器已装配时返回错误。
    pub async fn start_schedules(
        self: &Arc<Self>,
        executor: Arc<dyn ScheduleScriptExecutor>,
    ) -> RuntimeResult<()> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.schedule_mutation_gate.lock().await;
        if self.scheduler.is_configured() {
            return Err(RuntimeError::InternalStateUnavailable {
                component: "schedule executor already configured",
            });
        }
        let now = Utc::now();
        self.store
            .recover_schedule_executions(
                now.timestamp_millis(),
                executor.startup_cleanup_confirmed(),
            )
            .await
            .map_err(|error| RuntimeError::from_store("recover schedule executions", error))?;
        let mut offset = 0;
        let mut has_deadline = false;
        loop {
            let page = self
                .store
                .list_schedules(offset, 50)
                .await
                .map_err(|error| RuntimeError::from_store("recover schedule definitions", error))?;
            let count = page.schedules.len() as u32;
            for mut plan in page.schedules {
                if plan.deleting {
                    continue;
                }
                let due = next_local(&plan.settings, now)?
                    .as_ref()
                    .map(DateTime::timestamp_millis);
                has_deadline |= due.is_some();
                if plan.next_due_at_ms != due {
                    plan.next_due_at_ms = due;
                    if !self.store.replace_schedule(plan).await.map_err(|error| {
                        RuntimeError::from_store("recover schedule deadline", error)
                    })? {
                        return Err(RuntimeError::ScheduleNotFound);
                    }
                }
            }
            offset += count;
            if count == 0 || offset >= page.page.total {
                break;
            }
        }
        self.scheduler.executor.set(executor).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "schedule executor already configured",
            }
        })?;
        self.set_schedule_waiting(has_deadline);
        self.scheduler.maintenance.store(
            self.store
                .next_schedule_maintenance(None)
                .await
                .map_err(|error| RuntimeError::from_store("read schedule recovery", error))?
                .is_some(),
            Ordering::Release,
        );
        let observer = self.scheduler.changed.subscribe();
        let runtime = Arc::downgrade(self);
        let _ = self.scheduler.runtime.set(runtime.clone());
        let failure_owner = runtime.clone();
        let cancelled = self.root_cancellation.clone();
        self.tasks
            .spawn_passive(drive(runtime, observer, cancelled), async move {
                if let Some(runtime) = failure_owner.upgrade() {
                    eprintln!("event=schedule_supervisor_panicked");
                    runtime.set_schedule_waiting(false);
                }
            });
        Ok(())
    }

    pub(in crate::runtime) fn schedule_changed(&self) {
        if self.scheduler.is_configured() {
            // CRUD 持有计划门禁。先保活到下一轮可靠回读，避免最后一个客户端释放当前域。
            self.set_schedule_waiting(true);
            self.scheduler.changed.send_replace(());
        }
    }

    fn set_schedule_waiting(&self, waiting: bool) {
        if self.scheduler.waiting.swap(waiting, Ordering::AcqRel) != waiting {
            self.tasks.notify_work_changed();
        }
    }

    /// 手动请求与定时请求共用接纳事务。回执和正在执行的记录可安全读取，手动执行保留定时游标。
    ///
    /// # Errors
    /// 无计划、调度器未装配或存储失败时返回错误。
    pub async fn run_schedule_now(
        self: &Arc<Self>,
        request: RunScheduleNowRequest,
    ) -> RuntimeResult<RunScheduleNowResult> {
        let operation = self.operation_gate.clone().read_owned().await;
        self.ensure_running()?;
        let (reply, receipt) = tokio::sync::oneshot::channel();
        let runtime = self.clone();
        // HTTP 观察者断开时，已入队的 Store 接纳仍由同一 supervisor 负责进程登记。
        self.tasks.spawn(
            async move {
                let _operation = operation;
                let _ = reply.send(runtime.admit_manual_schedule(request).await);
            },
            async {},
        );
        receipt
            .await
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "manual schedule admission receipt",
            })?
    }

    async fn admit_manual_schedule(
        self: &Arc<Self>,
        request: RunScheduleNowRequest,
    ) -> RuntimeResult<RunScheduleNowResult> {
        let _mutation = self.schedule_mutation_gate.lock().await;
        self.ensure_running()?;
        match self
            .accept_schedule(
                request.schedule_id,
                ScheduleTrigger::Manual(request.idempotency_key),
                Utc::now(),
            )
            .await?
        {
            ScheduleAdmitted::Started(execution) => Ok(RunScheduleNowResult::Accepted {
                execution: execution.summary,
            }),
            ScheduleAdmitted::Replayed(execution) => {
                Ok(RunScheduleNowResult::Replayed { execution })
            }
            ScheduleAdmitted::Busy(execution) => Ok(RunScheduleNowResult::Busy { execution }),
            ScheduleAdmitted::Missing => Err(RuntimeError::ScheduleNotFound),
            ScheduleAdmitted::Ignored => Err(RuntimeError::InternalStateUnavailable {
                component: "manual schedule admission",
            }),
        }
    }

    /// 分页读取本用户一条计划的执行摘要，避免列表载入命令和输出。
    ///
    /// # Errors
    /// 分页无效、计划不存在或存储失败时返回错误。
    pub async fn list_schedule_executions(
        &self,
        request: ListScheduleExecutionsRequest,
    ) -> RuntimeResult<ListScheduleExecutionsResult> {
        let page = crate::runtime::context_page::PageWindow::new(request.page)?;
        self.store
            .list_schedule_executions(request.schedule_id, page.offset, page.limit)
            .await
            .map_err(|error| RuntimeError::from_store("list schedule executions", error))?
            .ok_or(RuntimeError::ScheduleNotFound)
    }

    /// 返回已可靠接纳的配置和当前用户的执行结果。
    ///
    /// # Errors
    /// 执行不存在或存储失败时返回错误。
    pub async fn get_schedule_execution(
        &self,
        request: ScheduleExecutionTarget,
    ) -> RuntimeResult<ScheduleExecution> {
        let mut execution = self
            .store
            .get_schedule_execution(request.execution_id)
            .await
            .map_err(|error| RuntimeError::from_store("get schedule execution", error))?
            .ok_or(RuntimeError::ScheduleExecutionNotFound)?;
        if let Some(delivery) = execution.delivery.as_ref() {
            let message_id = agent_types::MessageId::new(delivery.message_id.as_str().to_owned())
                .map_err(|_| RuntimeError::ScheduleExecutionNotFound)?;
            let location = self
                .store
                .locate_conversation_message(crate::ConversationMessageLocationRequest {
                    owner: assistant_protocol::ConversationOwner::MainSession {
                        session_id: delivery.session_id.clone(),
                    },
                    message_id,
                })
                .await
                .map_err(|error| RuntimeError::from_store("locate schedule report", error))?;
            if location
                .and_then(|location| location.display_ordinal)
                .is_none()
            {
                execution.delivery = None;
            }
        }
        if !execution.result_finalized {
            execution.result_error = self
                .scheduler
                .result_errors
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&execution.summary.execution_id)
                .cloned();
        }
        if let Some(id) = &execution.agent_session_id {
            execution.agent_run_status = execution
                .agent_result
                .as_ref()
                .and_then(|result| result.run_status);
            if let Some(session) = self.session_loader.cached(id)? {
                execution.agent_run_status = session.run_snapshots()?.first().map(|run| run.status);
            }
            execution.background_runs = self
                .background_runs
                .list(ListBackgroundRunsRequest {
                    session_id: id.clone(),
                    background_id: None,
                    status: None,
                    offset: None,
                    limit: Some(50),
                    include_command: None,
                })?
                .runs;
        }
        Ok(execution)
    }

    /// 请求取消。返回当前可靠状态，进程完成清理后由 supervisor 结算。
    ///
    /// # Errors
    /// 执行不存在、Runtime 无法接纳取消或读取失败时返回错误。
    pub async fn stop_schedule_execution(
        &self,
        request: ScheduleExecutionTarget,
    ) -> RuntimeResult<ScheduleExecutionSummary> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.schedule_mutation_gate.lock().await;
        let execution = self.get_schedule_execution(request).await?;
        if let Some((_, token)) = self
            .scheduler
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&execution.summary.execution_id)
        {
            token.cancel();
        }
        Ok(execution.summary)
    }

    /// 调用方持有生命周期与计划门禁。Store 事务复核当前游标和名额。进程登记与提交之间无异步取消点。
    /// 请求重试回读原记录，宿主崩溃由启动恢复标记未知，避免自动重放。
    async fn accept_schedule(
        self: &Arc<Self>,
        schedule_id: ScheduleId,
        trigger: ScheduleTrigger,
        now: DateTime<Utc>,
    ) -> RuntimeResult<ScheduleAdmitted> {
        let executor = self.scheduler.executor.get().cloned().ok_or(
            RuntimeError::InternalStateUnavailable {
                component: "schedule executor",
            },
        )?;
        let execution_id = ScheduleExecutionId::new(id::generate("scheduled").map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "schedule execution id generator",
            }
        })?)
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "schedule execution id generator",
        })?;
        let result = self
            .store
            .admit_schedule(ScheduleAdmission {
                schedule_id,
                execution_id,
                trigger,
                accepted_at_ms: now.timestamp_millis(),
                tracks_script_process: executor.records_process_identity(),
            })
            .await
            .map_err(|error| RuntimeError::from_store("admit schedule execution", error))?;
        if let ScheduleAdmitted::Started(execution) = &result {
            self.launch_schedule(execution, executor);
        }
        Ok(result)
    }

    /// 只为已提交的 running 记录启动受管任务。完成写入由任务负责，独立于 HTTP 连接与生命周期读门禁。
    fn launch_schedule(
        self: &Arc<Self>,
        execution: &ScheduleExecution,
        executor: Arc<dyn ScheduleScriptExecutor>,
    ) {
        let cancellation = self.root_cancellation.child_token();
        eprintln!(
            "event=schedule_execution_admitted execution_id={}",
            execution.summary.execution_id
        );
        let execution_id = execution.summary.execution_id.clone();
        self.scheduler
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                execution_id.clone(),
                (execution.summary.schedule_id.clone(), cancellation.clone()),
            );
        let runtime = self.clone();
        let on_panic = self.clone();
        let panic_id = execution_id.clone();
        let settings = execution.settings.clone();
        let schedule_id = execution.summary.schedule_id.clone();
        let script = matches!(settings.configuration.action, ScheduleAction::Script { .. });
        self.tasks.spawn(
            async move {
                let (status, result) = match &settings.configuration.action {
                    ScheduleAction::Script { command } => {
                        let workspace = settings
                            .configuration
                            .workspace_id
                            .as_ref()
                            .map(|id| runtime.workspace_for_new_session(id))
                            .transpose();
                        let (status, result) = match workspace {
                            Ok(project) => {
                                executor
                                    .execute(
                                        &schedule_id,
                                        &execution_id,
                                        project.as_ref(),
                                        command.clone(),
                                        cancellation,
                                    )
                                    .await
                            }
                            Err(_) => (
                                ScheduleExecutionStatus::Failed,
                                ScheduleScriptResult {
                                    reason: Some(ScheduleExecutionReason::WorkspaceUnavailable),
                                    ..Default::default()
                                },
                            ),
                        };
                        (status, ScheduleResult::Script(result))
                    }
                    ScheduleAction::Agent { .. } => {
                        let (status, result) = runtime
                            .execute_scheduled_agent(execution_id.clone(), settings, cancellation)
                            .await;
                        (status, ScheduleResult::Agent(result))
                    }
                };
                runtime.settle_execution(execution_id, status, result).await;
            },
            async move {
                on_panic
                    .settle_execution(
                        panic_id,
                        ScheduleExecutionStatus::CleanupUnconfirmed,
                        if script {
                            ScheduleResult::Script(ScheduleScriptResult {
                                reason: Some(ScheduleExecutionReason::CleanupUnconfirmed),
                                ..Default::default()
                            })
                        } else {
                            ScheduleResult::Agent(ScheduleAgentResult {
                                reason: Some(ScheduleExecutionReason::CleanupUnconfirmed),
                                ..Default::default()
                            })
                        },
                    )
                    .await;
            },
        );
    }

    /// 进程完成后只结算原执行标识。不取生命周期门禁，关闭流程等待本任务写完再释放 Store。
    /// 若落盘失败，保留可靠的 running 事实和失败标记，重启时收敛为未知。
    async fn settle_execution(
        &self,
        execution_id: ScheduleExecutionId,
        status: ScheduleExecutionStatus,
        result: ScheduleResult,
    ) {
        let saved = self
            .store
            .settle_schedule_execution(ScheduleSettlement {
                execution_id: execution_id.clone(),
                finished_at_ms: Utc::now().timestamp_millis(),
                status,
                result,
            })
            .await;
        if saved.is_err() || status.occupies_slot() {
            self.scheduler
                .cleanup_unconfirmed
                .store(true, Ordering::Release);
        }
        if saved.is_ok() {
            eprintln!(
                "event=schedule_execution_settled execution_id={execution_id} status={status:?}"
            );
        } else {
            eprintln!("event=schedule_execution_settlement_failed execution_id={execution_id}");
        }
        self.scheduler
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&execution_id);
        self.schedule_changed();
    }

    async fn schedule_step(self: &Arc<Self>, clock: &mut WakeClock) -> RuntimeResult<NextWait> {
        let _operation = tokio::select! {
            biased;
            () = self.root_cancellation.cancelled() => return Ok(NextWait::Stop),
            gate = self.operation_gate.read() => gate,
        };
        self.ensure_running()?;
        let _mutation = self.schedule_mutation_gate.lock().await;
        self.ensure_running()?;
        let next = self
            .store
            .next_schedule_due()
            .await
            .map_err(|error| RuntimeError::from_store("read schedule deadline", error))?;
        let Some(mut plan) = next else {
            self.set_schedule_waiting(false);
            return Ok(NextWait::Idle);
        };
        self.set_schedule_waiting(true);
        let at_ms = plan
            .next_due_at_ms
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "schedule deadline",
            })?;
        let now = Utc::now();
        clock.observe(now.timestamp_millis(), Instant::now());
        let before = DateTime::from_timestamp_millis(at_ms.saturating_sub(1)).ok_or(
            RuntimeError::InternalStateUnavailable {
                component: "schedule deadline",
            },
        )?;
        if next_local(&plan.settings, before)?
            .as_ref()
            .map(DateTime::timestamp_millis)
            != Some(at_ms)
        {
            // 设备时区发生变化时，旧绝对时间可能已不符合当前本地日历。
            // 当前持有定义门禁，先替换旧游标，下一轮再决定是否接纳。
            plan.next_due_at_ms = next_local(&plan.settings, now)?
                .as_ref()
                .map(DateTime::timestamp_millis);
            self.store.replace_schedule(plan).await.map_err(|error| {
                RuntimeError::from_store("recalculate schedule deadline", error)
            })?;
            return Ok(NextWait::Delay(Duration::ZERO));
        }
        let delay_ms = at_ms.saturating_sub(now.timestamp_millis());
        if delay_ms > 0 {
            return Ok(NextWait::Delay(
                Duration::from_millis(delay_ms as u64).min(CLOCK_RECHECK),
            ));
        }
        let next_at_ms = next_local(&plan.settings, now)?
            .as_ref()
            .map(DateTime::timestamp_millis);
        self.accept_schedule(
            plan.schedule_id,
            ScheduleTrigger::Due {
                at_ms,
                next_at_ms,
                missed: clock.missed(at_ms, now.timestamp_millis()),
            },
            now,
        )
        .await?;
        Ok(NextWait::Delay(Duration::ZERO))
    }
}

#[derive(Default)]
struct WakeClock {
    last: Option<(i64, Instant)>,
    skip_through: Option<i64>,
}

impl WakeClock {
    fn observe(&mut self, wall_ms: i64, monotonic: Instant) {
        if let Some((previous, started)) = self.last.replace((wall_ms, monotonic)) {
            let elapsed = i64::try_from(monotonic.saturating_duration_since(started).as_millis())
                .unwrap_or(i64::MAX);
            // 在睡眠不计入平台单调钟时，墙钟跃迁即使恰好落入短延迟窗也跳过。
            if wall_ms.saturating_sub(previous).saturating_sub(elapsed) > CLOCK_DRIFT_MS {
                self.skip_through = Some(wall_ms);
            } else if wall_ms < previous {
                self.skip_through = None;
            }
        }
    }

    fn missed(&self, due: i64, observed: i64) -> bool {
        self.skip_through.is_some_and(|cutoff| due <= cutoff)
            || observed.saturating_sub(due) > WAKE_GRACE_MS
    }
}

enum NextWait {
    Idle,
    Delay(Duration),
    Stop,
}

async fn drive(
    runtime: Weak<AssistantRuntime>,
    mut changed: watch::Receiver<()>,
    cancellation: CancellationToken,
) {
    let mut clock = WakeClock::default();
    let mut maintenance_cursor = None;
    loop {
        changed.borrow_and_update();
        let next = match runtime.upgrade() {
            Some(runtime) => {
                let timer = runtime.schedule_step(&mut clock).await;
                if matches!(timer, Ok(NextWait::Stop)) {
                    break;
                }
                let pending = runtime
                    .advance_schedule_maintenance(&mut maintenance_cursor)
                    .await
                    .unwrap_or(true);
                if runtime
                    .scheduler
                    .maintenance
                    .swap(pending, Ordering::AcqRel)
                    != pending
                {
                    runtime.tasks.notify_work_changed();
                }
                match timer {
                    Ok(NextWait::Idle) if pending => {
                        Ok(NextWait::Delay(Duration::from_millis(500)))
                    }
                    Ok(NextWait::Delay(delay)) if pending => {
                        Ok(NextWait::Delay(delay.min(Duration::from_millis(500))))
                    }
                    other => other,
                }
            }
            None => break,
        };
        let delay = match next {
            Ok(NextWait::Idle) => None,
            Ok(NextWait::Delay(delay)) => Some(delay),
            Ok(NextWait::Stop) => break,
            Err(_) if cancellation.is_cancelled() => break,
            Err(_) => {
                eprintln!("event=schedule_supervisor_retry");
                Some(CLOCK_RECHECK)
            }
        };
        // Tokio 时钟在部分系统休眠中暂停。单个有界等待定期按墙钟重算，跳过休眠跨过的窗口。
        tokio::select! {
            biased;
            () = cancellation.cancelled() => break,
            result = changed.changed() => if result.is_err() { break; },
            () = async { match delay { Some(delay) => tokio::time::sleep(delay).await, None => std::future::pending().await } } => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_wake_delay_preserves_original_time_but_resume_skips_crossed_deadlines() {
        let initial = Instant::now();
        let mut ordinary = WakeClock::default();
        ordinary.observe(40_000, initial);
        ordinary.observe(51_000, initial + Duration::from_secs(11));
        assert!(!ordinary.missed(50_000, 51_000));
        assert!(ordinary.missed(45_000, 51_000));

        let mut suspended = WakeClock::default();
        suspended.observe(40_000, initial);
        suspended.observe(51_000, initial + Duration::from_millis(200));
        assert!(suspended.missed(50_000, 51_000));
        assert!(suspended.missed(50_500, 51_001));
        suspended.observe(52_100, initial + Duration::from_millis(1300));
        assert!(!suspended.missed(52_000, 52_100));
    }
}
