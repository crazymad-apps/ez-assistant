//! 只读的可靠历史事实；这些值没有执行句柄，不能加入队列或恢复为活动任务。

mod fork;

use agent_types::MessageId;
use assistant_protocol::{
    AgentVariant, ApprovalMode, AttachmentId, ChildTaskId, ChildTaskStatus,
    FeedbackRequestSnapshot, InputId, MessageFeedback, ReasoningEffortKey, RunId, RunStatus,
    RuntimeErrorInfo, ToolCallId,
};
use serde::{Deserialize, Serialize};

use super::{StoredChildTask, StoredInput, StoredMcpSelection, StoredRun, StoredSessionCommand};
use crate::StoredSkillActivation;

/// 当前物理 owner 的文件历史读取结果；不包含执行恢复状态或消息正文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredConversationHistory {
    pub generation: u64,
    pub facts: StoredHistoryFacts,
}

/// 文件中一个本地或继承的 Run 的可靠历史值，不具备 `StoredRun` 的执行／恢复语义。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredRunHistory {
    pub run_id: RunId,
    /// 旧分支无法证明原始 Input 时保持未知，不伪造可重试的 Input。
    pub input_id: Option<InputId>,
    pub user_message_id: Option<MessageId>,
    pub attempt: u32,
    pub agent_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub agent_shell_kind: Option<assistant_protocol::ShellKind>,
    pub status: Option<RunStatus>,
    pub error: Option<RuntimeErrorInfo>,
    pub cancel_requested: bool,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub inherited: bool,
    /// 截点之后仍有执行内容时，不能显示源 Run 的未来终态或完整耗时。
    pub partial: bool,
    pub last_message_id: Option<MessageId>,
    /// 终结提交时本 owner 正文的可靠末条消息，可能属于另一次 Input。
    /// 无输出 retry 的结束位置不能借用旧 User；旧资料无法证明时保持未知。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_after_message_id: Option<MessageId>,
}

/// 已可靠进入历史的输入阅读事实；没有排队正文、幂等请求键或可执行状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredInputHistory {
    pub input_id: InputId,
    pub user_message_id: MessageId,
    pub queue_order: u64,
    pub agent_variant: AgentVariant,
    pub origin: super::InputOrigin,
    pub goal_binding: Option<super::GoalInputBinding>,
    pub cross_session: Option<super::CrossSessionInputEnvelope>,
    pub channel_source: Option<crate::InputChannelSource>,
    pub agent_shell_target: Option<assistant_protocol::ShellKind>,
    pub accepted_at_ms: i64,
    pub inherited: bool,
}

impl StoredInputHistory {
    /// 冻结实际 Input 的只读值，不复制其待消费载荷或恢复能力。
    pub fn from_input(input: &StoredInput) -> Self {
        Self {
            input_id: input.input_id.clone(),
            user_message_id: input.user_message_id.clone(),
            queue_order: input.queue_order,
            agent_variant: input.agent_variant,
            origin: input.origin,
            goal_binding: input.goal_binding.clone(),
            cross_session: input.cross_session.clone(),
            channel_source: input.channel_source.clone(),
            agent_shell_target: input.agent_shell_target,
            accepted_at_ms: input.accepted_at_ms,
            inherited: false,
        }
    }
}

/// 规范消息的可靠归属；缺失 Step 或 Input 的旧数据保留未知。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMessageOrigin {
    pub message_id: MessageId,
    pub run_id: Option<RunId>,
    pub input_id: Option<InputId>,
    pub attempt: Option<u32>,
    pub step: Option<u32>,
    pub inherited: bool,
}

/// child 历史的只读定位与终态；不含取消、审批、临时工作区或执行恢复状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredChildTaskHistory {
    pub child_task_id: ChildTaskId,
    pub parent_run_id: RunId,
    pub parent_tool_call_id: ToolCallId,
    pub title: String,
    pub agent_variant: AgentVariant,
    pub context_management_mode: agent_model::ContextManagementMode,
    pub context_window_tokens: Option<u64>,
    pub status: Option<ChildTaskStatus>,
    pub body_generation: u64,
    pub message_count: u64,
    pub final_message_id: Option<MessageId>,
    pub error: Option<RuntimeErrorInfo>,
    pub cancel_requested: bool,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub inherited: bool,
    /// Fork 保留了可靠描述，但未能复制源正文；不能以空文件替代或回源读取。
    #[serde(default)]
    pub body_unavailable: bool,
}

/// 绑定到可靠历史的受管附件；位置由 Host 校验，不能以历史记录授权任意文件读取。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredHistoryAttachment {
    pub attachment_id: AttachmentId,
    pub blob_hash: String,
    pub original_name: String,
    pub size_bytes: u64,
    pub media_type: Option<String>,
    pub relative_path: String,
}

/// 消息反馈的可靠变更；`None` 表示清除，不能被解码成“没有发生变更”。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredHistoryFeedback {
    pub message_id: assistant_protocol::MessageId,
    pub feedback: Option<MessageFeedback>,
    pub changed_at_ms: i64,
}

/// 一次可靠提交的历史变化，或 generation 替换时的归一化检查点。
///
/// owner 和 generation 由外层文件绑定。这里没有 SQL 键、文件 offset 或可执行 Run；
/// Host 文件帧和易失 Store 共用这些事实，Runtime 查询只做只读投影。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoredHistoryFacts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<StoredInputHistory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<StoredRunHistory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_origins: Vec<StoredMessageOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_tasks: Vec<StoredChildTaskHistory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<StoredSessionCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_activations: Vec<StoredSkillActivation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_selections: Vec<StoredMcpSelection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback: Vec<StoredHistoryFeedback>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<FeedbackRequestSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<StoredHistoryAttachment>,
}

impl StoredHistoryFacts {
    /// 按来源提交顺序合并变化；完成读取后调用 normalize 收敛重复事实。
    pub fn extend(&mut self, next: Self) {
        self.inputs.extend(next.inputs);
        self.runs.extend(next.runs);
        self.message_origins.extend(next.message_origins);
        self.child_tasks.extend(next.child_tasks);
        self.commands.extend(next.commands);
        self.skill_activations.extend(next.skill_activations);
        self.mcp_selections.extend(next.mcp_selections);
        self.feedback.extend(next.feedback);
        self.questions.extend(next.questions);
        self.attachments.extend(next.attachments);
    }

    /// 没有消息的提交必须至少包含一种可靠变化，避免空帧充当诊断日志。
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
            && self.runs.is_empty()
            && self.message_origins.is_empty()
            && self.child_tasks.is_empty()
            && self.commands.is_empty()
            && self.skill_activations.is_empty()
            && self.mcp_selections.is_empty()
            && self.feedback.is_empty()
            && self.questions.is_empty()
            && self.attachments.is_empty()
    }

    /// 按可靠提交顺序收敛同一事实的后续变化；保留最后一次完整值，不保留事件日志。
    ///
    /// 调用者先按文件顺序合并批次，本方法只对当前读取的值进行归一化，不写任何索引。
    pub fn normalize(&mut self) {
        retain_latest(&mut self.inputs, |value| value.input_id.clone());
        retain_latest(&mut self.runs, |value| value.run_id.clone());
        retain_latest(&mut self.message_origins, |value| value.message_id.clone());
        retain_latest(&mut self.child_tasks, |value| value.child_task_id.clone());
        retain_latest(&mut self.commands, |value| value.input_id.clone());
        retain_latest(&mut self.skill_activations, |value| {
            value.activation_id.clone()
        });
        retain_latest(&mut self.mcp_selections, |value| value.selection_id.clone());
        retain_latest(&mut self.feedback, |value| value.message_id.clone());
        retain_latest(&mut self.questions, |value| value.request_id.clone());
        retain_latest(&mut self.attachments, |value| value.attachment_id.clone());
    }
}

fn retain_latest<T, K: Eq + std::hash::Hash>(values: &mut Vec<T>, key: impl Fn(&T) -> K) {
    let mut seen = std::collections::HashSet::new();
    values.reverse();
    values.retain(|value| seen.insert(key(value)));
    values.reverse();
}

impl StoredRunHistory {
    /// 从已经核实归属的实际 Run 冻结阅读事实；不会因此创建新的执行身份。
    pub fn from_run(run: &StoredRun, user_message_id: Option<MessageId>) -> Self {
        Self {
            run_id: run.run_id.clone(),
            input_id: Some(run.input_id.clone()),
            user_message_id,
            attempt: run.attempt,
            agent_variant: run.agent_variant,
            approval_mode: run.approval_mode,
            reasoning_effort: run.reasoning_effort,
            agent_shell_kind: run.shell.as_ref().map(|shell| shell.kind),
            status: Some(run.status),
            error: run.error.clone(),
            cancel_requested: run.cancel_requested,
            created_at_ms: run.created_at_ms,
            started_at_ms: run.started_at_ms,
            finished_at_ms: run.finished_at_ms,
            inherited: false,
            partial: false,
            last_message_id: run.message_ids.last().cloned(),
            finished_after_message_id: None,
        }
    }
}

impl StoredChildTaskHistory {
    /// 只保存 child 阅读和父工具结果定位所需的冻结事实，不复制执行恢复状态。
    pub fn from_task(task: &StoredChildTask) -> Self {
        Self {
            child_task_id: task.child_task_id.clone(),
            parent_run_id: task.parent_run_id.clone(),
            parent_tool_call_id: task.parent_tool_call_id.clone(),
            title: task.title.clone(),
            agent_variant: task.agent_variant,
            context_management_mode: task.context_management_mode,
            context_window_tokens: task.context_window_tokens,
            status: Some(task.status),
            body_generation: task.body_generation,
            message_count: task.message_count,
            final_message_id: task.final_message_id.clone(),
            error: task.error.clone(),
            cancel_requested: task.cancel_requested,
            created_at_ms: task.created_at_ms,
            started_at_ms: task.started_at_ms,
            finished_at_ms: task.finished_at_ms,
            inherited: false,
            body_unavailable: task.conversation_state
                == super::StoredConversationState::Unavailable,
        }
    }
}
