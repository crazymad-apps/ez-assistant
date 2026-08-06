//! Runtime 可查询的只读业务快照和稳定状态。

use serde::{Deserialize, Serialize};

use crate::{RunId, RuntimeErrorInfo, SessionId, ToolCallId};

/// Runtime 对外可见的生命周期状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLifecycle {
    /// Runtime 正常接受业务操作。
    Running,
    /// Runtime 已拒绝新工作，正在取消并结算活动 Run。
    ShuttingDown,
    /// Runtime 已完成受控关闭。
    Stopped,
}

/// Runtime 业务 Run 的活动态和终态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Runtime 已原子登记 Run，但 supervisor 尚未开始执行。
    Accepted,
    /// AgentExecution 正在运行。
    Running,
    /// 已收到取消请求，正在等待执行可靠收敛。
    Cancelling,
    /// Run 正常完成。
    Completed,
    /// Run 执行失败。
    Failed,
    /// Run 已取消并完成结算。
    Cancelled,
    /// Core 要求上层进行上下文压缩；本版本不自动续跑。
    CompactionRequired,
}

impl RunStatus {
    /// 判断状态是否已经不可再次改变。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::CompactionRequired
        )
    }
}

/// 工具调用在 Run 观察投影中的状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityStatus {
    /// 模型已提出调用，但调用尚未开始执行。
    Proposed,
    /// 调用已通过授权并开始执行。
    Running,
    /// 调用已成功完成。
    Completed,
    /// 调用因拒绝、输入或执行错误而失败。
    Failed,
}

/// 工具流式输出的应用层通道。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputChannel {
    /// 标准输出。
    Stdout,
    /// 标准错误输出。
    Stderr,
}

/// 一个 Session 的稳定摘要。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    /// Session 的不透明标识。
    pub session_id: SessionId,
    /// 创建 Session 时确定的展示标题。
    pub title: String,
    /// 当前活动 Run；Session 空闲时为 `None`。
    pub active_run_id: Option<RunId>,
    /// 规范 Conversation 中已经完整提交的消息数量。
    pub message_count: u64,
}

/// Run 中一个工具调用的当前观察快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolActivitySnapshot {
    /// 工具调用的不透明标识。
    pub call_id: ToolCallId,
    /// 模型可见的工具名称。
    pub tool_name: String,
    /// 当前工具活动状态。
    pub status: ToolActivityStatus,
    /// 截至快照时观察到的标准输出；事件丢失时可能不完整。
    pub stdout: String,
    /// 截至快照时观察到的标准错误输出；事件丢失时可能不完整。
    pub stderr: String,
}

/// 一个 Runtime Run 的当前只读快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunSnapshot {
    /// Run 的不透明标识。
    pub run_id: RunId,
    /// Run 所属 Session。
    pub session_id: SessionId,
    /// 当前活动态或终态。
    pub status: RunStatus,
    /// 是否已经接受过取消请求。
    pub cancel_requested: bool,
    /// 截至快照时观察到的 reasoning；运行中可能因事件丢失而不完整。
    pub reasoning: String,
    /// 截至快照时观察到的正文；正常完成时由最终 AssistantMessage 校准。
    pub text: String,
    /// 当前已观察到的工具调用。
    pub tools: Vec<ToolActivitySnapshot>,
    /// Run 失败时可安全跨层展示的错误；其他状态为 `None`。
    pub error: Option<RuntimeErrorInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeErrorCode;

    #[test]
    fn run_status_serialization_and_terminal_semantics_are_stable() {
        let cases = [
            (RunStatus::Accepted, "accepted", false),
            (RunStatus::Running, "running", false),
            (RunStatus::Cancelling, "cancelling", false),
            (RunStatus::Completed, "completed", true),
            (RunStatus::Failed, "failed", true),
            (RunStatus::Cancelled, "cancelled", true),
            (RunStatus::CompactionRequired, "compaction_required", true),
        ];

        for (status, wire, terminal) in cases {
            assert_eq!(
                serde_json::to_string(&status).expect("serialize status"),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<RunStatus>(&format!("\"{wire}\""))
                    .expect("deserialize status"),
                status
            );
            assert_eq!(status.is_terminal(), terminal);
        }
    }

    #[test]
    fn supporting_status_enums_use_stable_snake_case_values() {
        let lifecycles = [
            (RuntimeLifecycle::Running, "running"),
            (RuntimeLifecycle::ShuttingDown, "shutting_down"),
            (RuntimeLifecycle::Stopped, "stopped"),
        ];
        for (value, wire) in lifecycles {
            assert_eq!(
                serde_json::to_string(&value).expect("serialize lifecycle"),
                format!("\"{wire}\"")
            );
        }

        let activities = [
            (ToolActivityStatus::Proposed, "proposed"),
            (ToolActivityStatus::Running, "running"),
            (ToolActivityStatus::Completed, "completed"),
            (ToolActivityStatus::Failed, "failed"),
        ];
        for (value, wire) in activities {
            assert_eq!(
                serde_json::to_string(&value).expect("serialize tool status"),
                format!("\"{wire}\"")
            );
        }

        assert_eq!(
            serde_json::to_string(&ToolOutputChannel::Stdout).expect("serialize stdout"),
            "\"stdout\""
        );
        assert_eq!(
            serde_json::to_string(&ToolOutputChannel::Stderr).expect("serialize stderr"),
            "\"stderr\""
        );
    }

    #[test]
    fn run_snapshot_round_trips_without_internal_runtime_state() {
        let snapshot = RunSnapshot {
            run_id: RunId::new("run-1").expect("run id"),
            session_id: SessionId::new("session-1").expect("session id"),
            status: RunStatus::Failed,
            cancel_requested: false,
            reasoning: "checked".to_owned(),
            text: "partial".to_owned(),
            tools: vec![ToolActivitySnapshot {
                call_id: ToolCallId::new("call-1").expect("call id"),
                tool_name: "echo_text".to_owned(),
                status: ToolActivityStatus::Completed,
                stdout: "hello".to_owned(),
                stderr: String::new(),
            }],
            error: Some(RuntimeErrorInfo::new(
                RuntimeErrorCode::Internal,
                "run failed",
            )),
        };

        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded: RunSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
        assert_eq!(decoded, snapshot);
    }
}
