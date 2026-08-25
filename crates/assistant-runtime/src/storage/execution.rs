use agent_core::ExchangeReceipt;
use agent_types::{
    AssistantMessage, ConversationMessage, MessageId, ToolMessage, TranscriptVisibility,
    UserMessage, UserMessageOrigin, UserPart,
};
use assistant_protocol::{
    AgentVariant, ApprovalMode, GoalId, IdempotencyKey, InputId, ReasoningEffortKey, RunId,
    RunStatus, RuntimeErrorInfo, SessionId, ToolCallId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::StoredGoal;
use crate::StoredSkillActivation;

/// 队列执行器领取一次 Run 时提交的 User Message 与结构化关联。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserMessageCommit {
    pub operation_id: String,
    pub input_id: InputId,
    pub run_id: RunId,
    pub session_id: SessionId,
    /// queued 输入首次开始时为 Some；已提交输入的新 attempt 不重复追加消息。
    pub message: Option<UserMessage>,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub created_at_ms: i64,
}

/// 工具副作用发生前必须可靠保存的完整 Assistant Tool Call 批次。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingToolExchange {
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub step: u32,
    pub assistant: AssistantMessage,
    pub created_at_ms: i64,
}

/// 工具结果齐备后，把 pending 批次整体转入规范 Conversation 的命令。
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedToolExchange {
    pub operation_id: String,
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub step: u32,
    pub results: Vec<ToolMessage>,
    /// 与结果同一可靠提交追加的隐藏 Runtime Skill 上下文。
    pub activation_message: Option<UserMessage>,
    /// 与隐藏消息同一事务写入的模型 Activation ledger。
    pub skill_activations: Vec<StoredSkillActivation>,
    pub completed_at_ms: i64,
}

/// Core 已获授权、即将进入工具副作用前的可靠 started 记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolExecutionStart {
    pub receipt: ExchangeReceipt,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub started_at_ms: i64,
}

/// Input 是否已经进入规范 Conversation。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredInputState {
    /// 正文暂存在结构化队列中，尚可取消。
    Queued,
    /// User Message 已提交到规范 Conversation，不再属于可取消队列。
    Committed,
}

/// Input 的业务创建者；与规范 UserMessage origin 正交但必须一致。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputOrigin {
    User,
    Runtime,
}

/// Input 与某一 Goal generation/turn 的冻结归属。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GoalInputBinding {
    pub goal_id: GoalId,
    pub generation: u64,
    pub turn: u32,
}

/// Runtime 从 Store 恢复的 Input 投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredInput {
    pub queue_order: u64,
    pub input_id: InputId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub agent_variant: AgentVariant,
    pub origin: InputOrigin,
    pub goal_binding: Option<GoalInputBinding>,
    /// 用户接受输入时冻结的单个 Skill；Runtime continuation 恒为空。
    pub skill_activation: Option<StoredSkillActivation>,
    pub user_message_id: MessageId,
    pub state: StoredInputState,
    pub queued_message: Option<UserMessage>,
    pub accepted_at_ms: i64,
}

/// 原子接受 Input 及其首次 Run 所需的完整事实。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewStoredInput {
    pub input_id: InputId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub agent_variant: AgentVariant,
    pub origin: InputOrigin,
    pub goal_binding: Option<GoalInputBinding>,
    /// 与 Input、首次 Run 和 queued message 同事务写入的用户 Skill Activation。
    pub skill_activation: Option<StoredSkillActivation>,
    pub approval_mode: ApprovalMode,
    pub message: UserMessage,
    /// 仅首次 start_goal 提供；Store 必须与 Input/Run 在同一事务中创建。
    pub new_goal: Option<StoredGoal>,
    /// 仅 resume_goal 提供；Store 必须 CAS 暂停 Goal 并与 Input/Run 原子提交。
    pub resumed_goal: Option<StoredGoal>,
    /// 首条输入可提供的有界自动标题；Store 仅在标题来源仍为系统生成时采用。
    pub generated_title: Option<String>,
    pub accepted_at_ms: i64,
}

/// Input 来源、Goal 归属或冻结 UserMessage 的组合不合法。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputMessageValidationError;

/// 校验 Input 来源、Goal 归属与冻结 UserMessage 的合法组合。
pub fn validate_input_message(
    origin: InputOrigin,
    goal_binding: Option<&GoalInputBinding>,
    message: &UserMessage,
) -> Result<(), InputMessageValidationError> {
    if goal_binding.is_some_and(|binding| binding.generation == 0 || binding.turn == 0) {
        return Err(InputMessageValidationError);
    }
    match origin {
        InputOrigin::User => {
            if message.origin != UserMessageOrigin::User
                || message.transcript_visibility != TranscriptVisibility::Visible
                || !message
                    .parts
                    .iter()
                    .any(|part| matches!(part, UserPart::Text(_) | UserPart::FileReferences(_)))
            {
                return Err(InputMessageValidationError);
            }
        }
        InputOrigin::Runtime => {
            if goal_binding.is_none()
                || message.origin != UserMessageOrigin::Runtime
                || message.transcript_visibility != TranscriptVisibility::Hidden
                || message.parts.is_empty()
                || message.parts.iter().any(|part| {
                    !matches!(part, UserPart::Injected(_) | UserPart::InternalContext(_))
                })
            {
                return Err(InputMessageValidationError);
            }
        }
    }
    Ok(())
}

/// Store 接受结果；幂等命中时返回首次持久化事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedInput {
    pub input: StoredInput,
    pub run: StoredRun,
    pub is_duplicate: bool,
}

/// 把一条仍在排队的 Input 提升为当前持久队列的下一项。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuePriorityChange {
    pub session_id: SessionId,
    pub input_id: InputId,
}

/// 从失败或中断 Run 创建下一次执行尝试的命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewStoredRunAttempt {
    pub run_id: RunId,
    pub source_run_id: RunId,
    pub session_id: SessionId,
    pub approval_mode: ApprovalMode,
    pub created_at_ms: i64,
}

/// 一次 Run 的可靠终态以及尚未写入正文的完整消息批次。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRunSettlement {
    pub operation_id: String,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub status: RunStatus,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub messages: Vec<ConversationMessage>,
    /// 本批最终 AssistantMessage 的可靠 step；旧调用方或无消息结算时为空。
    pub message_step: Option<u32>,
    pub goal_effect: Option<StoredGoalSettlementEffect>,
    pub finished_at_ms: i64,
}

/// Goal-bound Run 与 Goal/continuation 在同一 Store 提交中的转换。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredGoalSettlementEffect {
    Continue {
        expected_goal_id: GoalId,
        expected_generation: u64,
        goal: StoredGoal,
        next_input: Box<NewStoredInput>,
    },
    Transition {
        expected_goal_id: GoalId,
        expected_generation: u64,
        goal: StoredGoal,
        resume_required: bool,
    },
}

/// Store 原子结算后返回的 Goal 与可选后继权威投影。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoredRunSettlementResult {
    pub goal: Option<StoredGoal>,
    pub continuation: Option<AcceptedInput>,
    pub resume_required: bool,
}

/// Runtime 启动时恢复的 Run 结构化投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredRun {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub input_id: InputId,
    pub attempt: u32,
    pub status: RunStatus,
    pub agent_variant: AgentVariant,
    pub approval_mode: ApprovalMode,
    pub reasoning_effort: Option<ReasoningEffortKey>,
    pub cancel_requested: bool,
    pub error: Option<RuntimeErrorInfo>,
    pub message_ids: Vec<MessageId>,
    /// 新记录保存可靠消息所属 step；旧行缺失时不回填。
    pub message_steps: HashMap<MessageId, u32>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}
