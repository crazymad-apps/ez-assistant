//! 计划定义的校验、更新与查询。Store 是唯一权威，下一次本地时间在读取时派生。

mod agent;
mod calendar;
mod delivery;
pub(in crate::runtime) mod management_tools;
mod scheduler;

pub(super) use agent::{MATERIALIZATION_KEY_PREFIX, agent_visibility};
pub(crate) use agent::{ScheduledSession, is_scheduled_session};
pub(super) use delivery::report_source;
pub(super) use scheduler::Scheduler;
pub use scheduler::{ScheduleScriptExecutor, ScheduleScriptFuture};

use assistant_protocol::{
    BackgroundRunSummary, CreateScheduleRequest, GetWorkspaceRequest, ListBackgroundRunsRequest,
    ListSchedulesRequest, ListSchedulesResult, PreviewScheduleRequest, PreviewScheduleResult,
    ReadBackgroundOutputRequest, ReadBackgroundOutputResult, ReadScheduleBackgroundOutputRequest,
    ScheduleAction, ScheduleBackgroundTarget, ScheduleId, ScheduleSettings, ScheduleSnapshot,
    ScheduleTarget, SessionId, SetScheduleEnabledRequest, StopBackgroundRunRequest,
    StopBackgroundRunResult, UpdateScheduleRequest, WorkspaceId, WorkspaceLifecycle,
};
use chrono::{DateTime, Local, SecondsFormat, Utc};

use super::{AssistantRuntime, context_page::PageWindow};
use crate::{RuntimeError, RuntimeResult, StoreError, StoreErrorKind, StoredSchedule, id};
use calendar::CalendarRule;

const MAX_NAME_CHARS: usize = 80;
const MAX_ACTION_BYTES: usize = 64 * 1024;

impl AssistantRuntime {
    /// 查询当前用户和当前 Runtime 的有界计划页。
    ///
    /// # Errors
    /// 页参数、持久定义或 Store 不可用时返回错误。
    pub async fn list_schedules(
        &self,
        request: ListSchedulesRequest,
    ) -> RuntimeResult<ListSchedulesResult> {
        let window = PageWindow::new(request.page)?;
        let stored = self
            .store
            .list_schedules(window.offset, window.limit)
            .await
            .map_err(|error| RuntimeError::from_store("list schedules", error))?;
        let now = Utc::now();
        let scheduler = self.scheduler.is_configured();
        let schedules = stored
            .schedules
            .into_iter()
            .map(|schedule| snapshot(schedule, now, scheduler))
            .collect::<RuntimeResult<_>>()?;
        Ok(ListSchedulesResult {
            schedules,
            page: stored.page,
        })
    }

    /// 只按当前用户 Store 查询计划。
    ///
    /// # Errors
    /// ID 不存在或定义无法读取时返回错误。
    pub async fn get_schedule(&self, request: ScheduleTarget) -> RuntimeResult<ScheduleSnapshot> {
        snapshot(
            self.load_schedule(request.schedule_id).await?,
            Utc::now(),
            self.scheduler.is_configured(),
        )
    }

    /// 创建定义并在 Store 可靠提交后返回。先持有生命周期读门禁，再串行化定义变更。
    /// 随机 ID 碰撞只重试插入；重复的创建请求表示一条新计划。
    ///
    /// # Errors
    /// 定义、项目、Runtime 或 Store 不可用时返回错误。
    pub async fn create_schedule(
        &self,
        request: CreateScheduleRequest,
    ) -> RuntimeResult<ScheduleSnapshot> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.schedule_mutation_gate.lock().await;
        let settings = ScheduleSettings {
            configuration: request.configuration,
            report_to_controller: request.report_to_controller.unwrap_or(true),
        };
        self.validate_schedule_workspace(settings.configuration.workspace_id.as_ref())?;
        let (settings, next_run_at) = validate_settings(settings, None, Utc::now())?;
        let next_due_at_ms = next_run_at.as_ref().map(DateTime::timestamp_millis);
        let _notify = ScheduleNotifier(self);
        for _ in 0..id::GENERATION_ATTEMPTS {
            let schedule_id = id::generate("schedule")
                .ok()
                .and_then(|value| ScheduleId::new(value).ok())
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "schedule id generator",
                })?;
            let inserted = self
                .store
                .insert_schedule(StoredSchedule {
                    schedule_id: schedule_id.clone(),
                    deleting: false,
                    once_triggered: false,
                    settings: settings.clone(),
                    next_due_at_ms,
                })
                .await
                .map_err(|error| RuntimeError::from_store("create schedule", error))?;
            if inserted {
                return Ok(ScheduleSnapshot {
                    deleting: false,
                    once_triggered: false,
                    schedule_id,
                    settings,
                    next_run_at: next_run_at.map(format_local),
                });
            }
        }
        Err(RuntimeError::InternalStateUnavailable {
            component: "schedule id collision",
        })
    }

    /// 在同一变更门禁内核对编辑基线并整体替换定义。类型切换时不合并旧载荷。
    ///
    /// # Errors
    /// 不存在、定义无效或提交失败时返回错误。
    pub async fn update_schedule(
        &self,
        request: UpdateScheduleRequest,
    ) -> RuntimeResult<ScheduleSnapshot> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.schedule_mutation_gate.lock().await;
        let previous = self.load_schedule(request.schedule_id).await?;
        if request
            .expected_settings
            .as_deref()
            .is_some_and(|expected| expected != &previous.settings)
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "计划已被其他操作修改，请退出编辑后重新打开。",
            });
        }
        self.replace_schedule_settings(previous, request.settings)
            .await
    }

    /// 只更新启停状态。定义在计划门禁内读取，避免覆盖其他入口刚保存的编辑。
    ///
    /// # Errors
    /// 定义不存在、当前规则不能启用或提交失败时返回错误。
    pub async fn set_schedule_enabled(
        &self,
        request: SetScheduleEnabledRequest,
    ) -> RuntimeResult<ScheduleSnapshot> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let _mutation = self.schedule_mutation_gate.lock().await;
        let previous = self.load_schedule(request.schedule_id).await?;
        let mut settings = previous.settings.clone();
        settings.configuration.enabled = request.enabled;
        self.replace_schedule_settings(previous, settings).await
    }

    /// 使用执行设备的本地日历预览，不读写计划。
    ///
    /// # Errors
    /// 时间规则无效时返回错误。
    pub fn preview_schedule(
        &self,
        request: PreviewScheduleRequest,
    ) -> RuntimeResult<PreviewScheduleResult> {
        let rule = CalendarRule::parse(&request.rule)
            .map_err(|reason| RuntimeError::InvalidRequest { reason })?;
        Ok(PreviewScheduleResult {
            next_run_at: rule.next_after(&Local, Utc::now()).map(format_local),
        })
    }

    async fn replace_schedule_settings(
        &self,
        previous: StoredSchedule,
        settings: ScheduleSettings,
    ) -> RuntimeResult<ScheduleSnapshot> {
        if previous.deleting {
            return Err(RuntimeError::ScheduleBusy);
        }
        if settings.configuration.enabled
            || previous.settings.configuration.workspace_id != settings.configuration.workspace_id
        {
            self.validate_schedule_workspace(settings.configuration.workspace_id.as_ref())?;
        }
        let (settings, next_run_at) =
            validate_settings(settings, Some(&previous.settings), Utc::now())?;
        let next_due_at_ms = next_run_at.as_ref().map(DateTime::timestamp_millis);
        let once_triggered = previous.once_triggered
            && previous.settings.configuration.rule == settings.configuration.rule;
        let schedule_id = previous.schedule_id;
        let _notify = ScheduleNotifier(self);
        let replaced = self
            .store
            .replace_schedule(StoredSchedule {
                deleting: false,
                once_triggered,
                schedule_id: schedule_id.clone(),
                settings: settings.clone(),
                next_due_at_ms,
            })
            .await
            .map_err(|error| RuntimeError::from_store("update schedule", error))?;
        if !replaced {
            return Err(RuntimeError::ScheduleNotFound);
        }
        Ok(ScheduleSnapshot {
            deleting: false,
            once_triggered,
            schedule_id,
            settings,
            next_run_at: next_run_at.map(format_local),
        })
    }

    /// 先提交删除标记再取消 supervisor。持久标记允许 HTTP 断开或 Host 重启后继续清理。
    ///
    /// # Errors
    /// 计划不存在、Runtime 正在关闭、进程尚未回收或 Store 无法继续时返回错误。
    pub async fn delete_schedule(&self, request: ScheduleTarget) -> RuntimeResult<ScheduleTarget> {
        {
            let _operation = self.operation_gate.read().await;
            self.ensure_running()?;
            let _mutation = self.schedule_mutation_gate.lock().await;
            let _notify = ScheduleNotifier(self);
            let marked = self
                .store
                .mark_schedule_deleting(request.schedule_id.clone())
                .await
                .map_err(|error| RuntimeError::from_store("mark schedule deletion", error))?;
            if !marked {
                return Err(RuntimeError::ScheduleNotFound);
            }
            self.scheduler.cancel_plan(&request.schedule_id);
        }
        self.wait_for_schedule_deletion(&request.schedule_id)
            .await?;
        Ok(request)
    }

    async fn scheduled_background_owner(
        &self,
        target: &ScheduleBackgroundTarget,
    ) -> RuntimeResult<SessionId> {
        self.store
            .get_schedule_execution(target.execution_id.clone())
            .await
            .map_err(|error| RuntimeError::from_store("read scheduled background owner", error))?
            .and_then(|record| record.agent_session_id)
            .filter(is_scheduled_session)
            .ok_or(RuntimeError::BackgroundRunNotFound)
    }

    /// 查询执行名下当前受监控的后台进程。
    ///
    /// # Errors
    /// 执行记录或后台进程不可用时返回错误。
    pub async fn get_schedule_background(
        &self,
        target: ScheduleBackgroundTarget,
    ) -> RuntimeResult<BackgroundRunSummary> {
        let session_id = self.scheduled_background_owner(&target).await?;
        self.background_runs
            .list(ListBackgroundRunsRequest {
                session_id,
                background_id: Some(target.background_id),
                offset: None,
                limit: Some(1),
                status: None,
                include_command: Some(true),
            })?
            .runs
            .into_iter()
            .next()
            .ok_or(RuntimeError::BackgroundRunNotFound)
    }

    /// 按 F01 的输出游标读取当前执行所属的后台日志。
    ///
    /// # Errors
    /// 归属不匹配、进程结束或读取失败时返回错误。
    pub async fn read_schedule_background_output(
        &self,
        request: ReadScheduleBackgroundOutputRequest,
    ) -> RuntimeResult<ReadBackgroundOutputResult> {
        let session_id = self.scheduled_background_owner(&request.target).await?;
        self.background_runs
            .read(ReadBackgroundOutputRequest {
                session_id,
                background_id: request.target.background_id,
                cursor: request.cursor,
                offset_chars: request.offset_chars,
                limit_chars: request.limit_chars,
            })
            .await
    }

    /// 请求收回当前执行名下的一个后台进程。
    ///
    /// # Errors
    /// 归属不匹配、进程结束或未确认回收时返回错误。
    pub async fn stop_schedule_background(
        &self,
        target: ScheduleBackgroundTarget,
    ) -> RuntimeResult<StopBackgroundRunResult> {
        let session_id = self.scheduled_background_owner(&target).await?;
        self.background_services()
            .stop(StopBackgroundRunRequest {
                session_id,
                background_id: target.background_id,
            })
            .await
    }

    async fn load_schedule(&self, schedule_id: ScheduleId) -> RuntimeResult<StoredSchedule> {
        self.store
            .get_schedule(schedule_id)
            .await
            .map_err(|error| RuntimeError::from_store("get schedule", error))?
            .ok_or(RuntimeError::ScheduleNotFound)
    }

    fn validate_schedule_workspace(&self, workspace_id: Option<&WorkspaceId>) -> RuntimeResult<()> {
        if let Some(workspace_id) = workspace_id {
            let result = self.get_workspace(GetWorkspaceRequest {
                workspace_id: workspace_id.clone(),
            })?;
            if result.workspace.lifecycle == WorkspaceLifecycle::Removed {
                return Err(RuntimeError::WorkspaceRemoved {
                    workspace_id: workspace_id.clone(),
                });
            }
        }
        Ok(())
    }
}

fn validate_settings(
    mut settings: ScheduleSettings,
    previous: Option<&ScheduleSettings>,
    now: DateTime<Utc>,
) -> RuntimeResult<(ScheduleSettings, Option<DateTime<Local>>)> {
    let configuration = &mut settings.configuration;
    configuration.name = configuration.name.trim().to_owned();
    if configuration.name.is_empty() || configuration.name.chars().count() > MAX_NAME_CHARS {
        return Err(RuntimeError::InvalidRequest {
            reason: "schedule name must contain 1 to 80 characters",
        });
    }
    let body = match &configuration.action {
        ScheduleAction::Agent {
            instructions,
            custom_agent,
            model_selection,
            ..
        } => {
            if let Some(selection) = model_selection {
                crate::config::validation::validate_selection(selection)?;
            }
            if custom_agent
                .as_ref()
                .is_some_and(|selection| !crate::valid_custom_agent_name(&selection.name))
            {
                return Err(RuntimeError::InvalidRequest {
                    reason: "schedule custom agent name is invalid",
                });
            }
            instructions
        }
        ScheduleAction::Script { command } => command,
    };
    if body.trim().is_empty() || body.contains('\0') || body.len() > MAX_ACTION_BYTES {
        return Err(RuntimeError::InvalidRequest {
            reason: "schedule instructions or command must contain text within 64 KiB and no NUL characters",
        });
    }
    let rule = CalendarRule::parse(&configuration.rule)
        .map_err(|reason| RuntimeError::InvalidRequest { reason })?;
    let next = rule.next_after(&Local, now);
    // 已错过的一次性计划仍可编辑或暂停，新增、改时间和重新启用时须指定未来时间。
    if matches!(rule, CalendarRule::Once(_))
        && next.is_none()
        && !previous.is_some_and(|old| {
            old.configuration.rule == configuration.rule
                && (old.configuration.enabled || !configuration.enabled)
        })
    {
        return Err(RuntimeError::InvalidRequest {
            reason: "one-time schedule requires an available future local time",
        });
    }
    let next = next.filter(|_| configuration.enabled);
    Ok((settings, next))
}

fn snapshot(
    stored: StoredSchedule,
    now: DateTime<Utc>,
    scheduler_started: bool,
) -> RuntimeResult<ScheduleSnapshot> {
    let next = if stored.deleting {
        None
    } else if scheduler_started {
        stored
            .next_due_at_ms
            .and_then(DateTime::from_timestamp_millis)
            .map(|time| time.with_timezone(&Local))
    } else {
        next_local(&stored.settings, now)?
    };
    Ok(ScheduleSnapshot {
        schedule_id: stored.schedule_id,
        settings: stored.settings,
        deleting: stored.deleting,
        once_triggered: stored.once_triggered,
        next_run_at: next.map(format_local),
    })
}

fn next_local(
    settings: &ScheduleSettings,
    now: DateTime<Utc>,
) -> RuntimeResult<Option<DateTime<Local>>> {
    let rule = CalendarRule::parse(&settings.configuration.rule).map_err(|reason| {
        RuntimeError::from_store(
            "read schedule",
            StoreError::new(StoreErrorKind::InvalidData, reason),
        )
    })?;
    Ok(settings
        .configuration
        .enabled
        .then(|| rule.next_after(&Local, now))
        .flatten())
}

fn format_local(time: DateTime<Local>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, false)
}

// HTTP 连接可能在 Store 已接纳写命令后关闭。释放计划门禁前通知调度器回读，
// 同一个 worker 会先完成已入队的写入，再执行调度查询。
struct ScheduleNotifier<'a>(&'a AssistantRuntime);

impl Drop for ScheduleNotifier<'_> {
    fn drop(&mut self) {
        self.0.schedule_changed();
    }
}
