//! Goal 用户命令与跨 Run 结算的 Runtime 用例层。

mod commands;
mod settlement;

pub(super) use commands::{GoalSubmissionPersistence, PreparedGoalSubmission};
pub(crate) use settlement::prepare_goal_run_settlement;
