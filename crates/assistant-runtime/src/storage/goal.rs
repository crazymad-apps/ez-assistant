//! Goal 控制器的持久化边界 DTO。

use agent_types::{FileReferencesPart, MessageId, TextPart, UserMessage};
use assistant_protocol::{GoalId, InputId, RunId, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "part", rename_all = "snake_case")]
pub enum StoredGoalObjectivePart {
    Text(TextPart),
    FileReferences(FileReferencesPart),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredGoalObjective {
    pub source_message_id: MessageId,
    pub payload: Vec<StoredGoalObjectivePart>,
    pub payload_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredGoalState {
    Running,
    Paused,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredGoalPauseReason {
    Blocked { summary: String },
    UserStopped,
    RunLimitReached,
    TokenLimitReached,
    ConsecutiveFailures,
    RecoveryRequired,
    Forked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredGoalBudget {
    pub max_runs: u32,
    pub max_total_tokens: u64,
    pub max_consecutive_failures: u32,
    pub used_runs: u32,
    pub used_total_tokens: u64,
    pub usage_complete: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredGoal {
    pub goal_id: GoalId,
    pub session_id: SessionId,
    pub objective: StoredGoalObjective,
    pub state: StoredGoalState,
    pub pause_reason: Option<StoredGoalPauseReason>,
    pub generation: u64,
    pub turn: u32,
    pub budget: StoredGoalBudget,
    pub consecutive_failures: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

/// 用户停止当前 Goal 所需的 CAS 事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalStop {
    pub session_id: SessionId,
    pub goal_id: GoalId,
    pub expected_generation: u64,
    pub stopped_goal: StoredGoal,
}

/// Stop 事务对排队 continuation 与活动 Run 的权威处理结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalStopResult {
    pub goal: StoredGoal,
    pub removed_input_ids: Vec<InputId>,
    pub cancelling_run_id: Option<RunId>,
}

/// 清除暂停或已完成 Goal 控制器所需的 CAS 事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalClear {
    pub session_id: SessionId,
    pub goal_id: GoalId,
    pub expected_generation: u64,
}

/// 将一条 held 用户 Input 原子绑定到恢复后的新 Goal generation。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalHeldInputResume {
    pub session_id: SessionId,
    pub input_id: InputId,
    pub expected_goal_id: GoalId,
    pub expected_generation: u64,
    pub resumed_goal: StoredGoal,
    pub message: UserMessage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalHeldInputResumeResult {
    pub goal: StoredGoal,
    pub input: crate::StoredInput,
    pub run: crate::StoredRun,
}
