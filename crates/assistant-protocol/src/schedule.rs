//! 用户 Runtime 内的计划配置和查询投影。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ApprovalMode, ContextPage, ContextPageRequest, CustomAgentSelection, IdempotencyKey,
    ModelSelection, ScheduleExecutionId, ScheduleId, WorkspaceId,
};

/// 所有日历字段均使用执行设备的本地时间。日期格式为 YYYY-MM-DD，时间格式为 HH:MM。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ScheduleRule {
    Once {
        date: String,
        time: String,
    },
    Daily {
        time: String,
    },
    Weekly {
        weekdays: Vec<ScheduleWeekday>,
        time: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ScheduleWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

/// 两种配置整体替换。脚本型只接收命令文本，Agent 型可沿用 Runtime 的默认智能体或模型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ScheduleAction {
    Agent {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        custom_agent: Option<CustomAgentSelection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        model_selection: Option<ModelSelection>,
        approval_mode: ApprovalMode,
    },
    Script {
        command: String,
    },
}

/// 计划的公共信息和互斥执行配置。汇报开关的创建默认值由创建命令处理。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleConfiguration {
    pub name: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace_id: Option<WorkspaceId>,
    pub rule: ScheduleRule,
    pub action: ScheduleAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct CreateScheduleRequest {
    #[serde(flatten)]
    pub configuration: ScheduleConfiguration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub report_to_controller: Option<bool>,
}

/// 可编辑的完整定义。更新时必须明确传入是否向主控汇报。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleSettings {
    #[serde(flatten)]
    pub configuration: ScheduleConfiguration,
    pub report_to_controller: bool,
}

/// 下次时间带执行设备的 UTC 偏移，供跨设备客户端显示当地时间。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleSnapshot {
    #[serde(default)]
    pub deleting: bool,
    /// 当前一次性规则曾产生定时记录，区别于关机或休眠期间错过。
    #[serde(default)]
    pub once_triggered: bool,
    pub schedule_id: ScheduleId,
    pub settings: ScheduleSettings,
    pub next_run_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleTarget {
    pub schedule_id: ScheduleId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct UpdateScheduleRequest {
    pub schedule_id: ScheduleId,
    pub settings: ScheduleSettings,
    /// 编辑器可提交打开编辑时的定义，避免覆盖随后保存的修改。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_settings: Option<Box<ScheduleSettings>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SetScheduleEnabledRequest {
    pub schedule_id: ScheduleId,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PreviewScheduleRequest {
    pub rule: ScheduleRule,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PreviewScheduleResult {
    pub next_run_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListSchedulesRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page: Option<ContextPageRequest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListSchedulesResult {
    pub schedules: Vec<ScheduleSnapshot>,
    pub page: ContextPage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct RunScheduleNowRequest {
    pub schedule_id: ScheduleId,
    pub idempotency_key: IdempotencyKey,
}

/// replayed 返回此前接纳的执行。busy 返回占用该计划执行名额的记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum RunScheduleNowResult {
    Accepted { execution: ScheduleExecutionSummary },
    Replayed { execution: ScheduleExecutionSummary },
    Busy { execution: ScheduleExecutionSummary },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ScheduleExecutionStatus {
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    Skipped,
    OutcomeUnknown,
    CleanupUnconfirmed,
}

impl ScheduleExecutionStatus {
    pub fn occupies_slot(self) -> bool {
        matches!(
            self,
            Self::Running | Self::OutcomeUnknown | Self::CleanupUnconfirmed
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "assistant-protocol.ts")]
pub enum ScheduleExecutionReason {
    PreviousExecutionActive,
    WorkspaceUnavailable,
    ShellUnavailable,
    Signalled,
    HostInterrupted,
    CleanupUnconfirmed,
    Cancelled,
    AgentPreparationFailed,
    AgentRunFailed,
    AgentApprovalRequired,
    BackgroundFailed,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleScriptResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub reason: Option<ScheduleExecutionReason>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleExecutionSummary {
    pub execution_id: ScheduleExecutionId,
    pub schedule_id: ScheduleId,
    pub accepted_at_ms: i64,
    /// 手动执行为空。定时执行使用原定瞬间作为接纳身份。
    pub scheduled_for_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub status: ScheduleExecutionStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleExecution {
    #[serde(flatten)]
    pub summary: ScheduleExecutionSummary,
    pub settings: ScheduleSettings,
    pub script_result: Option<ScheduleScriptResult>,
    pub agent_result: Option<ScheduleAgentResult>,
    pub agent_session_id: Option<crate::SessionId>,
    pub agent_run_status: Option<crate::RunStatus>,
    #[serde(default)]
    pub background_runs: Vec<crate::BackgroundRunSummary>,
    #[serde(default)]
    pub files: Vec<ScheduleResultFile>,
    #[serde(default)]
    pub delivery: Option<ScheduleDelivery>,
    #[serde(default)]
    pub result_finalized: bool,
    /// 最近一次本进程尝试的失败原因。等待主控空闲属于正常处理状态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result_error: Option<String>,
}

/// 执行详情和已提交的主控消息共享同一受管文件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleResultFile {
    pub file_id: String,
    pub name: String,
    pub title: String,
    pub size_bytes: u64,
}

/// 主控历史中的可靠定位，仅在消息提交后出现。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleDelivery {
    pub session_id: crate::SessionId,
    pub message_id: crate::MessageId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleExecutionTarget {
    pub execution_id: ScheduleExecutionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleBackgroundTarget {
    pub execution_id: ScheduleExecutionId,
    pub background_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ReadScheduleBackgroundOutputRequest {
    #[serde(flatten)]
    pub target: ScheduleBackgroundTarget,
    #[serde(default)]
    #[ts(optional)]
    pub cursor: Option<u64>,
    #[serde(default)]
    #[ts(optional)]
    pub offset_chars: Option<i64>,
    #[serde(default)]
    #[ts(optional)]
    pub limit_chars: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListScheduleExecutionsRequest {
    pub schedule_id: ScheduleId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page: Option<ContextPageRequest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListScheduleExecutionsResult {
    pub executions: Vec<ScheduleExecutionSummary>,
    pub page: ContextPage,
}

/// 仅从计划执行读取的 Agent 收尾事实。声明的路径仍相对于任务会话资源根。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleAgentResult {
    #[serde(default)]
    pub run_status: Option<crate::RunStatus>,
    pub report: Option<String>,
    pub artifacts: Vec<crate::SessionArtifact>,
    pub background_exits: Vec<ScheduleBackgroundExit>,
    pub reason: Option<ScheduleExecutionReason>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ScheduleBackgroundExit {
    pub background_id: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stopped: bool,
}
