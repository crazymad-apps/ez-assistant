//! 计划后处理只重做持久游标指向的阶段。删除标记与消息提交共用计划变更门禁。

use agent_types::{
    InternalContextPart, PartId, TextPart, TranscriptVisibility, UserMessage, UserMessageOrigin,
    UserPart,
};
use assistant_protocol::{
    ConversationInputSourceSnapshot, InputId, MessageId, ScheduleExecution, ScheduleExecutionId,
    ScheduleExecutionStatus, ScheduleExecutionTarget, ScheduleId,
};
use tokio::time;

use crate::{
    AssistantRuntime, CrossSessionInputBinding, CrossSessionInputEnvelope, InputOrigin,
    NewStoredInput, ResolvedScheduleFile, RuntimeError, RuntimeResult, ScheduleMaintenance,
    ScheduleReportCommit,
};

const REPORT_KIND: &str = "schedule_report";

impl AssistantRuntime {
    /// 真实文件路径只交给当前用户域 Host。Store 复核执行或主控消息的归属，再解析清单。
    pub async fn resolve_schedule_result_file(
        &self,
        execution_id: ScheduleExecutionId,
        file_id: String,
    ) -> RuntimeResult<ResolvedScheduleFile> {
        self.store
            .resolve_schedule_file(execution_id, file_id)
            .await
            .map_err(|error| RuntimeError::from_store("resolve schedule result file", error))?
            .ok_or(RuntimeError::ScheduleExecutionNotFound)
    }

    /// 同一轮最多处理有限项。临时失败保留游标后的其他项，下一轮再补做。
    pub(super) async fn advance_schedule_maintenance(
        &self,
        cursor: &mut Option<String>,
    ) -> RuntimeResult<bool> {
        let _operation = tokio::select! {
            biased;
            () = self.root_cancellation.cancelled() => return Ok(false),
            gate = self.operation_gate.read() => gate,
        };
        self.ensure_running()?;
        let mut pending = false;
        for _ in 0..16 {
            let work = self
                .store
                .next_schedule_maintenance(cursor.clone())
                .await
                .map_err(|error| RuntimeError::from_store("read schedule maintenance", error))?;
            let Some((key, work)) = work else {
                *cursor = None;
                break;
            };
            *cursor = Some(key);
            pending = true;
            let outcome = match work {
                ScheduleMaintenance::Deletion(id) => self.try_remove_schedule(id).await.map(|_| ()),
                ScheduleMaintenance::Result(id) => self.attempt_schedule_result(id).await,
            };
            if let Err(error) = outcome {
                eprintln!("event=schedule_maintenance_pending error={error}");
            }
        }
        // 游标已到尾时仍复核一次队列头，确保重启、临时 I/O 失败和控制器忙碌均会重试。
        if !pending {
            pending = self
                .store
                .next_schedule_maintenance(None)
                .await
                .map_err(|error| {
                    RuntimeError::from_store("read pending schedule maintenance", error)
                })?
                .is_some();
        }
        Ok(pending)
    }

    /// 只推进已经结算的执行后处理。归档和投递继续复用持久幂等游标。
    ///
    /// # Errors
    /// 执行尚未结算、记录不存在或任一后处理步骤失败时返回错误。
    pub async fn retry_schedule_result(
        &self,
        request: ScheduleExecutionTarget,
    ) -> RuntimeResult<ScheduleExecution> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        let execution = self.get_schedule_execution(request.clone()).await?;
        if execution.summary.status == ScheduleExecutionStatus::Running {
            return Err(RuntimeError::ScheduleBusy);
        }
        if !execution.result_finalized {
            self.attempt_schedule_result(request.execution_id.clone())
                .await?;
        }
        self.get_schedule_execution(request).await
    }

    async fn attempt_schedule_result(&self, id: ScheduleExecutionId) -> RuntimeResult<()> {
        let outcome = self.prepare_schedule_delivery(id.clone()).await;
        let reason = outcome.as_ref().err().and_then(|error| match error {
            RuntimeError::SessionNotIdle { .. } => None,
            RuntimeError::StorageUnavailable { operation, .. } => Some(
                match *operation {
                    "archive schedule result" => "保存执行结果失败。请重试归档。",
                    "commit schedule report" => "主控汇报提交失败。请重试投递。",
                    "release scheduled session" => "临时任务资源清理失败。请重试收尾。",
                    _ => "暂时无法读取执行结果的持久状态。",
                }
                .into(),
            ),
            other => Some(other.to_protocol_info().message),
        });
        self.scheduler.note_result_attempt(id, reason);
        outcome
    }

    async fn prepare_schedule_delivery(&self, id: ScheduleExecutionId) -> RuntimeResult<()> {
        // 文件写入由 Store worker 负责，完成前没有可打开的清单。
        self.store
            .archive_schedule_result(id.clone())
            .await
            .map_err(|error| RuntimeError::from_store("archive schedule result", error))?;
        let _mutation = self.schedule_mutation_gate.lock().await;
        let Some(execution) = self
            .store
            .get_schedule_execution(id.clone())
            .await
            .map_err(|error| RuntimeError::from_store("read schedule result to deliver", error))?
        else {
            return Ok(());
        };
        if !self
            .store
            .get_schedule(execution.summary.schedule_id.clone())
            .await
            .map_err(|error| RuntimeError::from_store("read schedule delivery owner", error))?
            .is_some_and(|plan| !plan.deleting)
        {
            return Ok(());
        }
        if execution.settings.report_to_controller
            && execution.summary.status != ScheduleExecutionStatus::Skipped
        {
            self.deliver_schedule_report(&execution).await?;
        }
        self.store
            .finish_schedule_result(id)
            .await
            .map_err(|error| RuntimeError::from_store("release scheduled session", error))?;
        Ok(())
    }

    /// 结果入队和回执共用 Store 事务。旧回执只用于恢复已有队列，不补跑旧通知。
    async fn deliver_schedule_report(&self, execution: &ScheduleExecution) -> RuntimeResult<()> {
        let controller = if let Some(delivery) = &execution.delivery {
            self.prepared_session(&delivery.session_id).await?
        } else {
            let owner = self.ensure_controller_session(true).await?;
            self.prepared_session(owner.id()).await?
        };
        let mutation = controller.mutation().await;
        controller.ensure_healthy()?;
        let variant = controller.lock_state()?.current_variant;
        let id = &execution.summary.execution_id;
        let message_id = MessageId::new(format!("scheduled-report-{id}"))
            .map_err(|_| report_identity_error())?;
        let accepted = self
            .store
            .commit_schedule_report(ScheduleReportCommit {
                execution_id: id.clone(),
                input: Box::new(NewStoredInput {
                    input_id: InputId::new(message_id.as_str())
                        .map_err(|_| report_identity_error())?,
                    session_id: controller.id().clone(),
                    idempotency_key: None,
                    agent_variant: variant,
                    origin: InputOrigin::Runtime,
                    goal_binding: None,
                    cross_session: Some(CrossSessionInputEnvelope {
                        binding: CrossSessionInputBinding::ScheduleReport {
                            execution_id: id.clone(),
                        },
                        reply_route: crate::ReplyRoute::SessionDefault,
                    }),
                    channel_source: None,
                    skill_activation: None,
                    mcp_selection: None,
                    feedback_answer: None,
                    agent_shell_target: None,
                    message: make_report(execution, &message_id)?,
                    new_goal: None,
                    resumed_goal: None,
                    generated_title: None,
                    accepted_at_ms: super::super::now_ms()?,
                }),
            })
            .await
            .map_err(|error| RuntimeError::from_store("commit schedule report", error))?;
        drop(mutation);
        if let Some(accepted) = accepted {
            self.controller_tool_coordinator()
                .project_controller_report(accepted)
                .await?;
        }
        Ok(())
    }

    /// 调用方不持计划门禁。仅在监督者移除该计划的活动令牌后清理其私有资源。
    pub(super) async fn try_remove_schedule(&self, id: ScheduleId) -> RuntimeResult<bool> {
        let _mutation = self.schedule_mutation_gate.lock().await;
        if self.scheduler.cancel_plan(&id) {
            return Ok(false);
        }
        self.store
            .delete_schedule(id)
            .await
            .map_err(|error| RuntimeError::from_store("finish schedule deletion", error))
    }

    pub(super) async fn wait_for_schedule_deletion(&self, id: &ScheduleId) -> RuntimeResult<()> {
        time::timeout(self.config.shutdown_timeout, async {
            loop {
                let _operation = self.operation_gate.read().await;
                self.ensure_running()?;
                // 主动驱动一次，也允许定时监督者抢先完成同一个已提交删除。
                if self.try_remove_schedule(id.clone()).await?
                    || self
                        .store
                        .get_schedule(id.clone())
                        .await
                        .map_err(|error| {
                            RuntimeError::from_store("verify schedule deletion", error)
                        })?
                        .is_none()
                {
                    return Ok(());
                }
                drop(_operation);
                time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| RuntimeError::ScheduleBusy)?
    }
}

pub(in crate::runtime) fn report_source(
    message: &UserMessage,
) -> Option<ConversationInputSourceSnapshot> {
    if !matches!(
        message.origin,
        UserMessageOrigin::HostNotice | UserMessageOrigin::Runtime
    ) {
        return None;
    }
    message.parts.iter().find_map(|part| match part {
        UserPart::InternalContext(part) if part.kind == REPORT_KIND => {
            serde_json::from_str(&part.text).ok()
        }
        _ => None,
    })
}

fn make_report(
    execution: &ScheduleExecution,
    message_id: &MessageId,
) -> RuntimeResult<UserMessage> {
    let status = execution.summary.status;
    let mut text = format!(
        "定时任务「{}」 · {}",
        execution.settings.configuration.name,
        status_text(status)
    );
    let started = execution
        .summary
        .scheduled_for_ms
        .unwrap_or(execution.summary.accepted_at_ms);
    if let Some(started) = chrono::DateTime::from_timestamp_millis(started) {
        use std::fmt::Write as _;
        let _ = write!(
            text,
            "\n执行时间 {}",
            super::format_local(started.with_timezone(&chrono::Local))
        );
    }
    if let Some(result) = &execution.script_result {
        use std::fmt::Write as _;
        if let Some(code) = result.exit_code {
            let _ = write!(text, "\n退出码 {code}");
        }
        if result.truncated {
            text.push_str("\n输出已截断。");
        }
        if let Some(reason) = result.reason {
            let _ = write!(text, "\n原因 {reason:?}");
        }
        text.push_str("\n\n标准输出\n");
        append_report_excerpt(&mut text, &result.stdout, 2000);
        text.push_str("\n\n标准错误\n");
        append_report_excerpt(&mut text, &result.stderr, 2000);
    }
    if let Some(result) = &execution.agent_result {
        if let Some(report) = &result.report {
            text.push_str("\n\n");
            append_report_excerpt(&mut text, report, 6000);
        } else if let Some(reason) = result.reason {
            use std::fmt::Write as _;
            let _ = write!(text, "\n原因 {reason:?}");
        }
    }
    let id = agent_types::MessageId::new(message_id.as_str().to_owned())
        .map_err(|_| report_identity_error())?;
    let mut message = UserMessage {
        id,
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Visible,
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new(format!("{message_id}-text")).map_err(|_| report_identity_error())?,
            text,
        })],
    };
    let source = ConversationInputSourceSnapshot::ScheduleReport {
        execution_id: execution.summary.execution_id.clone(),
        files: execution.files.clone(),
    };
    let content = serde_json::to_string(&source).map_err(|_| report_identity_error())?;
    let part = InternalContextPart::new(
        PartId::new(format!("{message_id}-metadata")).map_err(|_| report_identity_error())?,
        message_id.as_str(),
        REPORT_KIND,
        content,
    )
    .map_err(|_| report_identity_error())?;
    message.parts.push(UserPart::InternalContext(part));
    Ok(message)
}

fn append_report_excerpt(text: &mut String, source: &str, limit: usize) {
    if let Some((end, _)) = source.char_indices().nth(limit) {
        text.push_str(&source[..end]);
        text.push_str("\n\n（此处为节选，完整结果见执行记录。）");
    } else {
        text.push_str(source);
    }
}

fn report_identity_error() -> RuntimeError {
    RuntimeError::InternalStateUnavailable {
        component: "schedule report identity",
    }
}

fn status_text(status: ScheduleExecutionStatus) -> &'static str {
    use ScheduleExecutionStatus::*;
    match status {
        Running => "执行中",
        Succeeded => "成功",
        Failed => "失败",
        TimedOut => "超时",
        Cancelled => "已停止",
        Skipped => "已跳过",
        OutcomeUnknown => "结果未知",
        CleanupUnconfirmed => "清理未确认",
    }
}
