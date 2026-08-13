//! Agent 变体与审批模式的 SQLite 稳定字符串转换。

use assistant_protocol::{AgentVariant, ApprovalMode, ChildTaskStatus};

use super::{StorageResult, invalid_data};

pub(super) fn agent_variant_value(value: AgentVariant) -> &'static str {
    match value {
        AgentVariant::Plan => "plan",
        AgentVariant::Build => "build",
    }
}

pub(super) fn parse_agent_variant(value: &str) -> StorageResult<AgentVariant> {
    match value {
        "plan" => Ok(AgentVariant::Plan),
        "build" => Ok(AgentVariant::Build),
        _ => Err(invalid_data("stored agent variant is invalid")),
    }
}

pub(super) fn approval_mode_value(value: ApprovalMode) -> &'static str {
    match value {
        ApprovalMode::Ask => "ask",
        ApprovalMode::Auto => "auto",
    }
}

pub(super) fn parse_approval_mode(value: &str) -> StorageResult<ApprovalMode> {
    match value {
        "ask" => Ok(ApprovalMode::Ask),
        "auto" => Ok(ApprovalMode::Auto),
        _ => Err(invalid_data("stored approval mode is invalid")),
    }
}

pub(super) fn child_task_status_value(value: ChildTaskStatus) -> &'static str {
    match value {
        ChildTaskStatus::Accepted => "accepted",
        ChildTaskStatus::Running => "running",
        ChildTaskStatus::Completed => "completed",
        ChildTaskStatus::Failed => "failed",
        ChildTaskStatus::Cancelled => "cancelled",
        ChildTaskStatus::Interrupted => "interrupted",
    }
}

pub(super) fn parse_child_task_status(value: &str) -> StorageResult<ChildTaskStatus> {
    match value {
        "accepted" => Ok(ChildTaskStatus::Accepted),
        "running" => Ok(ChildTaskStatus::Running),
        "completed" => Ok(ChildTaskStatus::Completed),
        "failed" => Ok(ChildTaskStatus::Failed),
        "cancelled" => Ok(ChildTaskStatus::Cancelled),
        "interrupted" => Ok(ChildTaskStatus::Interrupted),
        _ => Err(invalid_data("stored child task status is invalid")),
    }
}
