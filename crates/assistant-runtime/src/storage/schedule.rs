use assistant_protocol::{ContextPage, ScheduleId, ScheduleSettings};

/// 用户 Store 的计划定义。配置沿用版本化的计划契约，下一次时间由 Runtime 按设备日历派生。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSchedule {
    pub deleting: bool,
    pub once_triggered: bool,
    pub schedule_id: ScheduleId,
    pub settings: ScheduleSettings,
    pub next_due_at_ms: Option<i64>,
}

/// 同一次 Store 读取中的有界定义和总数。
pub struct StoredSchedulePage {
    pub schedules: Vec<StoredSchedule>,
    pub page: ContextPage,
}

/// 同一执行接纳事务中的手动请求或定时游标。过久的到期点只推进游标。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduleTrigger {
    Manual(assistant_protocol::IdempotencyKey),
    Due {
        at_ms: i64,
        next_at_ms: Option<i64>,
        missed: bool,
    },
}

/// Runtime 已取得候选时间和生命周期门禁。Store 复核定义与名额并原子落账。
#[derive(Clone, Debug)]
pub struct ScheduleAdmission {
    pub schedule_id: ScheduleId,
    pub execution_id: assistant_protocol::ScheduleExecutionId,
    pub accepted_at_ms: i64,
    pub trigger: ScheduleTrigger,
    pub tracks_script_process: bool,
}

pub enum ScheduleAdmitted {
    Started(Box<assistant_protocol::ScheduleExecution>),
    Replayed(assistant_protocol::ScheduleExecutionSummary),
    Busy(assistant_protocol::ScheduleExecutionSummary),
    Ignored,
    Missing,
}

#[derive(Clone, Debug)]
pub struct ScheduleSettlement {
    pub execution_id: assistant_protocol::ScheduleExecutionId,
    pub finished_at_ms: i64,
    pub status: assistant_protocol::ScheduleExecutionStatus,
    pub result: ScheduleResult,
}

impl ScheduleAdmission {
    /// 冻结接纳时的定义。定时重叠仍保存本次原定时间和跳过原因。
    pub fn execution(
        &self,
        settings: ScheduleSettings,
        skipped: bool,
    ) -> assistant_protocol::ScheduleExecution {
        use assistant_protocol::{
            ScheduleExecution, ScheduleExecutionReason, ScheduleExecutionStatus,
            ScheduleExecutionSummary, ScheduleScriptResult,
        };
        let scheduled_for_ms = match self.trigger {
            ScheduleTrigger::Due { at_ms, .. } => Some(at_ms),
            ScheduleTrigger::Manual(_) => None,
        };
        let reason = Some(ScheduleExecutionReason::PreviousExecutionActive);
        let script = matches!(
            settings.configuration.action,
            assistant_protocol::ScheduleAction::Script { .. }
        );
        ScheduleExecution {
            summary: ScheduleExecutionSummary {
                execution_id: self.execution_id.clone(),
                schedule_id: self.schedule_id.clone(),
                accepted_at_ms: self.accepted_at_ms,
                scheduled_for_ms,
                finished_at_ms: skipped.then_some(self.accepted_at_ms),
                status: if skipped {
                    ScheduleExecutionStatus::Skipped
                } else {
                    ScheduleExecutionStatus::Running
                },
            },
            settings,
            script_result: (skipped && script).then_some(ScheduleScriptResult {
                reason,
                ..Default::default()
            }),
            agent_result: (skipped && !script).then_some(assistant_protocol::ScheduleAgentResult {
                reason,
                ..Default::default()
            }),
            agent_session_id: None,
            agent_run_status: None,
            background_runs: Vec::new(),
            files: Vec::new(),
            delivery: None,
            result_finalized: false,
            result_error: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ScheduleResult {
    Script(assistant_protocol::ScheduleScriptResult),
    Agent(assistant_protocol::ScheduleAgentResult),
}

/// Runtime 的计划后处理游标。两个来源独立分页，失败只延后对应项。
#[derive(Clone, Debug)]
pub enum ScheduleMaintenance {
    Deletion(ScheduleId),
    Result(assistant_protocol::ScheduleExecutionId),
}

/// Store 使用与消息正文相同的提交边界保存该执行的送达回执。
pub struct ScheduleReportCommit {
    pub execution_id: assistant_protocol::ScheduleExecutionId,
    pub input: Box<crate::NewStoredInput>,
}

pub struct ResolvedScheduleFile {
    pub path: std::path::PathBuf,
    pub file: assistant_protocol::ScheduleResultFile,
}
