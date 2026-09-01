//! Goal 用户命令、AgentExecution 门禁与最终结算的 Runtime 用例层。

mod commands;
mod settlement;

pub(super) use commands::{
    GoalSubmissionPersistence, PreparedGoalSubmission, ensure_goal_model_supported,
    prepare_goal_start,
};
pub(crate) use settlement::{GoalRunDecision, GoalRunFacts, prepare_goal_run_decision};
