//! Safety Demo 私有 HTTP 请求、快照与 SSE DTO。
//!
//! 这些类型只服务当前开发工具，不进入 `assistant-protocol`。

use agent_types::ConversationMessage;
use serde::{Deserialize, Serialize};

use crate::{
    approval::{ApprovalDecision, PendingApprovalSnapshot},
    audit::AuditEntry,
    policy::{ApprovalMode, ExecutionMode},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
/// Demo Run 对页面公开的稳定终态与活动态。
pub(crate) enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    CompactionRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// 单个已启用 Guardrail 检测器的冻结配置。
pub(crate) struct GuardrailCheckSnapshot {
    pub mode: agent_core::ActiveGuardrailMode,
    pub threshold: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// 当前 Run 显式采用的两类 Guardrail 配置。
pub(crate) struct GuardrailSettingsSnapshot {
    pub repeated_invocation: GuardrailCheckSnapshot,
    pub consecutive_failures: GuardrailCheckSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// 最近一次 Guardrail 触发事件的页面投影。
pub(crate) struct GuardrailTriggerSnapshot {
    pub kind: agent_core::GuardrailKind,
    pub mode: agent_core::ActiveGuardrailMode,
    pub threshold: u32,
    pub observed: u32,
    pub call_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// 当前或最近一次 Run 的权威状态快照。
pub(crate) struct RunSnapshot {
    pub run_id: String,
    pub status: RunStatus,
    pub execution_mode: ExecutionMode,
    pub approval_mode: ApprovalMode,
    pub cancel_requested: bool,
    pub event_count: u64,
    pub guardrails: GuardrailSettingsSnapshot,
    pub last_guardrail: Option<GuardrailTriggerSnapshot>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
/// 页面首次加载和断线恢复时整体替换本地状态的权威 Session 快照。
pub(crate) struct SessionSnapshot {
    pub session_id: String,
    pub session_workdir: String,
    pub temporary_workspace: String,
    pub active_run: bool,
    pub run: Option<RunSnapshot>,
    pub pending_approval: bool,
    pub approval: Option<PendingApprovalSnapshot>,
    pub is_resetting: bool,
    pub journal_entries: usize,
    pub journal: Vec<ConversationMessage>,
    pub audit_entries: usize,
    pub audit: Vec<AuditEntry>,
    /// 当前已发布的最后一个事件序号；初始快照为 0。
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// 启动下一次 Run 的私有 HTTP 请求。
pub(crate) struct StartRunRequest {
    pub message: String,
    pub execution_mode: ExecutionMode,
    pub approval_mode: ApprovalMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// 回答指定 pending approval 的私有 HTTP 请求。
pub(crate) struct ApprovalDecisionRequest {
    pub decision: ApprovalDecision,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// SSE 只发送失效通知；页面收到后仍以 snapshot 为权威。
pub(crate) struct EventNotification {
    pub sequence: u64,
    pub kind: EventKind,
}

/// 页面实时展示所需的脱敏 Core 事件投影。
///
/// ToolProposed 不携带原始 arguments，避免把 write/edit 文件内容复制到活动日志；
/// 页面在工具完成后仍以权威 Journal 和审计快照校准结果。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RunProgressDetail {
    StepStarted {
        step: u32,
    },
    UsageUpdated {
        step: u32,
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
    TextDelta {
        part_id: String,
        delta: String,
    },
    ReasoningDelta {
        part_id: String,
        delta: String,
    },
    ToolProposed {
        call_id: String,
        tool_name: String,
    },
    ToolStarted {
        call_id: String,
    },
    ToolOutput {
        call_id: String,
        channel: String,
        chunk: String,
    },
    ToolCompleted {
        call_id: String,
        status: String,
    },
    GuardrailTriggered {
        kind: agent_core::GuardrailKind,
        mode: agent_core::ActiveGuardrailMode,
        threshold: u32,
        observed: u32,
        call_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum EventKind {
    SessionReset,
    RunStarted {
        run_id: String,
    },
    RunProgress {
        run_id: String,
        event: String,
        detail: Option<RunProgressDetail>,
    },
    RunFinished {
        run_id: String,
        status: RunStatus,
    },
    ApprovalChanged {
        approval_id: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct GapNotification {
    pub skipped: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ApiErrorBody {
    pub code: &'static str,
    pub message: &'static str,
}
