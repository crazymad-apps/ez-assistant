//! Runtime 可查询的只读业务快照和稳定状态。

use serde::{Deserialize, Serialize};

use crate::{
    AttachmentId, InputId, ModelKey, RunId, RuntimeErrorInfo, SessionId, ToolCallId, WorkspaceId,
};

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

/// Session 是否仍可接受业务变更。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    /// 可提交输入、重试、重新输入和切换模型。
    Active,
    /// 只允许查询，等待显式恢复。
    Archived,
}

/// Session 列表的生命周期过滤条件。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionListFilter {
    /// 只返回活动 Session。
    #[default]
    Active,
    /// 只返回归档 Session。
    Archived,
    /// 返回全部 Session。
    All,
}

/// Workspace 是否仍可供新 Session 选择。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceLifecycle {
    /// Workspace 正常登记，可绑定新 Session。
    Active,
    /// Workspace 已从正常选择列表移除，但历史绑定和目录均保留。
    Removed,
}

/// 一个 Workspace 的稳定业务投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceSummary {
    /// Runtime 分配的不透明 Workspace 标识。
    pub workspace_id: WorkspaceId,
    /// Host canonicalize 后保存的用户工作目录。
    pub user_directory: String,
    /// Runtime Home 中由 Host 管理的 Workspace 级 Agent 私有目录。
    pub agent_directory: String,
    /// Workspace 当前是否可供新 Session 选择。
    pub lifecycle: WorkspaceLifecycle,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub removed_at_ms: Option<i64>,
}

/// Attachment 的物理正文和稳定视图当前是否可用。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentState {
    Ready,
    Unavailable,
}

/// 一个 Session Attachment 的客户端可见业务投影。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttachmentSummary {
    pub attachment_id: AttachmentId,
    pub session_id: SessionId,
    pub original_name: String,
    pub size_bytes: u64,
    pub agent_readable_path: String,
    pub state: AttachmentState,
    pub created_at_ms: i64,
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
    /// Runtime 重启前没有可靠终结；不会自动恢复执行。
    Interrupted,
    /// Core 要求上层进行上下文压缩；本版本不自动续跑。
    CompactionRequired,
}

impl RunStatus {
    /// 判断状态是否已经不可再次改变。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
                | Self::CompactionRequired
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
    /// Session 后续 Run 当前使用的用户模型 key。
    pub model_key: ModelKey,
    /// Session 当前是活动还是归档状态。
    pub lifecycle: SessionLifecycle,
    /// 创建 Session 时冻结的 Workspace 绑定；`None` 表示普通未绑定会话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// 当前活动 Run；Session 空闲时为 `None`。
    pub active_run_id: Option<RunId>,
    /// 规范 Conversation 中已经完整提交的消息数量。
    pub message_count: u64,
    /// 尚未进入规范 Conversation 的持久化输入数量。
    pub queued_input_count: u64,
    /// 重启恢复后队列是否等待用户显式继续。
    pub resume_required: bool,
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

/// 一次完整模型请求由 Provider 最终确认的 token 用量。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsageSnapshot {
    /// 本次请求消耗的输入 token。
    pub input_tokens: u64,
    /// 本次响应产生的输出 token。
    pub output_tokens: u64,
    /// Provider 报告的总 token；不在应用层重新计算。
    pub total_tokens: u64,
    /// 输入 token 中命中缓存的数量；Provider 未提供时为 `None`。
    pub cached_input_tokens: Option<u64>,
}

/// 一个 Runtime Run 的当前只读快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunSnapshot {
    /// Run 的不透明标识。
    pub run_id: RunId,
    /// Run 所属 Session。
    pub session_id: SessionId,
    /// 本次 Run 所属的持久化输入。
    pub input_id: InputId,
    /// 同一输入的执行尝试序号，从 1 开始。
    pub attempt: u32,
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
            (RunStatus::Interrupted, "interrupted", true),
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

        let session_lifecycles = [
            (SessionLifecycle::Active, "active"),
            (SessionLifecycle::Archived, "archived"),
        ];
        for (value, wire) in session_lifecycles {
            assert_eq!(
                serde_json::to_string(&value).expect("serialize session lifecycle"),
                format!("\"{wire}\"")
            );
        }
        assert_eq!(SessionListFilter::default(), SessionListFilter::Active);

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
            input_id: InputId::new("input-1").expect("input id"),
            attempt: 1,
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
