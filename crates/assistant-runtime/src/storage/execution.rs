use agent_core::ExchangeReceipt;
use agent_types::{
    AssistantMessage, ConversationMessage, MessageId, ToolMessage, TranscriptVisibility,
    UserMessage, UserMessageOrigin, UserPart,
};
use assistant_protocol::{
    AgentVariant, ApprovalMode, GoalId, IdempotencyKey, InputId, McpRefreshControlResultSnapshot,
    McpServerKey, ReasoningEffortKey, RunId, RunStatus, RuntimeErrorInfo, SessionCommand,
    SessionId, ToolCallId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::StoredGoal;
use crate::StoredSkillActivation;
use crate::{InputChannelSource, ReplyRoute};

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
    /// 由跨 Session 投递启动的 Goal 冻结其最终回复路径；普通 Goal 为 `None`。
    pub reply_route: Option<ReplyRoute>,
}

/// 可见 Runtime Input 的冻结跨会话关联事实。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CrossSessionInputBinding {
    /// 主控某次 Tool Call 向普通 Session 发起的任务输入。
    ControllerDelivery {
        /// 发起投递的主控 Session。
        controller_session_id: SessionId,
        /// 当前投递所属的主控 Run；必须仍处于执行期。
        controller_run_id: RunId,
        /// 用于整个投递操作幂等的主控 Tool Call。
        controller_tool_call_id: ToolCallId,
    },
    /// 普通 Session 在代理任务链结束后返回主控的结构化报告来源。
    ProxyReport {
        /// 实际执行任务的普通 Session。
        source_session_id: SessionId,
        /// 触发本次报告的终态 Run。
        source_run_id: RunId,
        /// 该 Run 所属的 Goal；单轮任务为 `None`。
        source_goal_id: Option<GoalId>,
        /// 来源 Run 已可靠提交的终态。
        source_run_status: RunStatus,
    },
}

/// 跨 Session Input 的持久消息信封。
///
/// `binding` 只描述当前消息的来源与关联身份；`reply_route` 是整条代理请求链共同携带的
/// `reply-to`，由 `ControllerDelivery` 冻结并在 `ProxyReport` 实际输出前解析。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CrossSessionInputEnvelope {
    /// 当前跨会话消息的来源类型和关联身份。
    pub binding: CrossSessionInputBinding,
    /// 从原始主控输入冻结并沿代理链传递的最终回复路径。
    pub reply_route: ReplyRoute,
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
    pub cross_session: Option<CrossSessionInputEnvelope>,
    pub channel_source: Option<InputChannelSource>,
    /// 用户接受输入时冻结的单个 Skill；Runtime continuation 恒为空。
    pub skill_activation: Option<StoredSkillActivation>,
    pub user_message_id: MessageId,
    pub state: StoredInputState,
    pub queued_message: Option<UserMessage>,
    pub accepted_at_ms: i64,
}

/// MCP Selection 的可靠小型关系事实；不允许复制动态 Tool Catalog 或连接状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredMcpSelection {
    pub selection_id: String,
    pub session_id: SessionId,
    pub input_id: Option<InputId>,
    pub message_id: MessageId,
    pub server_key: McpServerKey,
    pub display_name: String,
    pub created_at_ms: i64,
}

impl StoredMcpSelection {
    pub(crate) fn tag(&self) -> assistant_protocol::McpSelectionTagSnapshot {
        assistant_protocol::McpSelectionTagSnapshot {
            server_key: self.server_key.clone(),
            display_name: self.display_name.clone(),
        }
    }
}

/// Session Command 的持久状态。执行中不落单独状态，崩溃后仍从 Queued 恢复。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredSessionCommandState {
    Queued,
    Committed,
}

/// Runtime Store 恢复出的结构化 Session Command；它没有对应 Run。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredSessionCommand {
    pub queue_order: u64,
    pub input_id: InputId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub user_message_id: MessageId,
    pub agent_variant: AgentVariant,
    pub command: SessionCommand,
    pub result: Option<McpRefreshControlResultSnapshot>,
    pub state: StoredSessionCommandState,
    pub accepted_at_ms: i64,
}

/// 同一可靠队列中的互斥载荷；Message 有首次 Run，Command 永远没有 Run。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredQueueItem {
    Message(Box<StoredInput>),
    Command(StoredSessionCommand),
}

/// 后续 `accept_session_command` 所需的完整事实；M0 只固定 Store 边界类型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewStoredSessionCommand {
    pub input_id: InputId,
    pub session_id: SessionId,
    pub idempotency_key: Option<IdempotencyKey>,
    pub user_message_id: MessageId,
    pub agent_variant: AgentVariant,
    pub command: SessionCommand,
    pub accepted_at_ms: i64,
}

/// Store 原子接纳 Session Command 后返回的可靠行与幂等命中状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedStoredSessionCommand {
    pub command: StoredSessionCommand,
    pub is_duplicate: bool,
}

/// 后续 `commit_session_command` 的原子结算事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionCommandCommit {
    pub operation_id: String,
    pub input_id: InputId,
    pub session_id: SessionId,
    pub result: McpRefreshControlResultSnapshot,
    pub message: UserMessage,
    pub committed_at_ms: i64,
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
    pub cross_session: Option<CrossSessionInputEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_source: Option<InputChannelSource>,
    /// 与 Input、首次 Run 和 queued message 同事务写入的用户 Skill Activation。
    pub skill_activation: Option<StoredSkillActivation>,
    /// 与 Input、首次 Run 同事务写入的单 Server 手选事实。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_selection: Option<StoredMcpSelection>,
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
    cross_session: Option<&CrossSessionInputEnvelope>,
    message: &UserMessage,
) -> Result<(), InputMessageValidationError> {
    let channel_source = match origin {
        InputOrigin::User => Some(InputChannelSource::desktop_text()),
        InputOrigin::Runtime => None,
    };
    validate_input_message_with_channel_source(
        origin,
        goal_binding,
        cross_session,
        channel_source.as_ref(),
        message,
    )
}

pub fn validate_input_message_with_channel_source(
    origin: InputOrigin,
    goal_binding: Option<&GoalInputBinding>,
    cross_session: Option<&CrossSessionInputEnvelope>,
    channel_source: Option<&InputChannelSource>,
    message: &UserMessage,
) -> Result<(), InputMessageValidationError> {
    if goal_binding.is_some_and(|binding| binding.generation == 0 || binding.turn == 0) {
        return Err(InputMessageValidationError);
    }
    match (origin, cross_session, channel_source) {
        (InputOrigin::User, None, Some(channel_source)) => {
            if matches!(channel_source, InputChannelSource::Device(source) if source.client_input_id.trim().is_empty())
            {
                return Err(InputMessageValidationError);
            }
            if message.origin != UserMessageOrigin::User
                || message.transcript_visibility != TranscriptVisibility::Visible
                || !message.parts.iter().any(|part| {
                    matches!(
                        part,
                        UserPart::Text(_) | UserPart::FileReferences(_) | UserPart::QuotedText(_)
                    )
                })
            {
                return Err(InputMessageValidationError);
            }
        }
        (InputOrigin::Runtime, None, None) => {
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
        (InputOrigin::Runtime, Some(envelope), None) => {
            let expected_kind = match &envelope.binding {
                CrossSessionInputBinding::ControllerDelivery { .. } => "controller_delivery",
                CrossSessionInputBinding::ProxyReport {
                    source_run_status, ..
                } => {
                    if !matches!(
                        source_run_status,
                        RunStatus::Completed
                            | RunStatus::Failed
                            | RunStatus::Cancelled
                            | RunStatus::Interrupted
                    ) {
                        return Err(InputMessageValidationError);
                    }
                    "proxy_report"
                }
            };
            if goal_binding.is_some_and(|binding| {
                !matches!(
                    envelope.binding,
                    CrossSessionInputBinding::ControllerDelivery { .. }
                ) || binding.reply_route.as_ref() != Some(&envelope.reply_route)
            }) {
                return Err(InputMessageValidationError);
            }
            let source_parts = message
                .parts
                .iter()
                .filter(|part| {
                    matches!(part, UserPart::InternalContext(part) if part.kind == expected_kind)
                })
                .count();
            if message.origin != UserMessageOrigin::Runtime
                || message.transcript_visibility != TranscriptVisibility::Visible
                || !message
                    .parts
                    .iter()
                    .any(|part| matches!(part, UserPart::Text(_)))
                || source_parts != 1
            {
                return Err(InputMessageValidationError);
            }
        }
        (InputOrigin::User, Some(_), _)
        | (InputOrigin::User, None, None)
        | (InputOrigin::Runtime, _, Some(_)) => {
            return Err(InputMessageValidationError);
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
    /// 源 Run 结算时，同事务可靠接受到 Controller 的可见代理报告。
    pub proxy_report: Option<Box<NewStoredInput>>,
    pub finished_at_ms: i64,
}

/// 活动 Run 在启动下一次 AgentExecution 前可靠提交的消息与可选 Goal 变化。
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRunContinuation {
    pub operation_id: String,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub messages: Vec<ConversationMessage>,
    /// 本批最终 AssistantMessage 的可靠 step；只有隐藏消息时仍沿用刚完成的 step。
    pub message_step: u32,
    pub goal_effect: Option<StoredGoalSettlementEffect>,
    pub committed_at_ms: i64,
}

/// 活动 Run 续跑提交后的权威业务投影。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoredRunContinuationResult {
    pub goal: Option<StoredGoal>,
    pub resume_required: bool,
}

/// Goal-bound Run 在同一 Store 提交中应用的领域变化。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredGoalSettlementEffect {
    Progress {
        expected_goal_id: GoalId,
        expected_generation: u64,
        goal: StoredGoal,
    },
    Transition {
        expected_goal_id: GoalId,
        expected_generation: u64,
        goal: StoredGoal,
        resume_required: bool,
    },
}

/// Store 原子结算后返回的 Goal 与代理报告权威投影。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoredRunSettlementResult {
    pub goal: Option<StoredGoal>,
    pub accepted_proxy_report: Option<AcceptedInput>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_protocol::{McpRefreshOutcome, McpServerRefreshOutcome};

    #[test]
    fn stored_session_command_round_trips_without_a_run_identity() {
        let command = StoredSessionCommand {
            queue_order: 7,
            input_id: InputId::new("command-1").expect("input"),
            session_id: SessionId::new("session-1").expect("session"),
            idempotency_key: Some(IdempotencyKey::new("request-1").expect("key")),
            user_message_id: MessageId::new("message-1").expect("message"),
            agent_variant: AgentVariant::Build,
            command: SessionCommand::McpRefresh {
                server: Some(McpServerKey::new("github").expect("server")),
            },
            result: Some(McpRefreshControlResultSnapshot {
                outcome: McpRefreshOutcome::Success,
                servers: vec![assistant_protocol::McpServerRefreshResultSnapshot {
                    server_key: McpServerKey::new("github").expect("server"),
                    outcome: McpServerRefreshOutcome::Refreshed,
                    tool_count: 12,
                    diagnostic: None,
                }],
            }),
            state: StoredSessionCommandState::Committed,
            accepted_at_ms: 10,
        };
        let json = serde_json::to_string(&command).expect("serialize");
        assert_eq!(
            serde_json::from_str::<StoredSessionCommand>(&json).expect("deserialize"),
            command
        );
        assert!(!json.contains("run_id"));
    }

    #[test]
    fn stored_mcp_selection_contains_only_stable_label_facts() {
        let selection = StoredMcpSelection {
            selection_id: "selection-1".to_owned(),
            session_id: SessionId::new("session-1").expect("session"),
            input_id: Some(InputId::new("input-1").expect("input")),
            message_id: MessageId::new("message-1").expect("message"),
            server_key: McpServerKey::new("github").expect("server"),
            display_name: "GitHub".to_owned(),
            created_at_ms: 10,
        };
        let json = serde_json::to_string(&selection).expect("serialize");
        assert_eq!(
            serde_json::from_str::<StoredMcpSelection>(&json).expect("deserialize"),
            selection
        );
        assert!(!json.contains("tools"));
        assert!(!json.contains("catalog"));
    }
}
