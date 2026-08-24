//! WorkPlan 的持久化边界 DTO 与原子变更命令。

use assistant_protocol::{SessionId, TodoItemId};
use serde::{Deserialize, Serialize};

/// Store 中一个待办项的稳定状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredTodoItemStatus {
    Pending,
    InProgress,
    Completed,
}

/// Store 中一个扁平 WorkPlan 待办项。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredWorkPlanItem {
    pub id: TodoItemId,
    pub text: String,
    pub status: StoredTodoItemStatus,
}

/// Store 中一个 Session 当前唯一的 WorkPlan 快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredWorkPlan {
    pub session_id: SessionId,
    pub revision: u64,
    pub objective: String,
    pub items: Vec<StoredWorkPlanItem>,
    pub last_operation_id: String,
    pub updated_at_ms: i64,
}

/// 以完整替换语义提交 WorkPlan；Store 负责 CAS 与 operation 幂等。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkPlanMutation {
    pub session_id: SessionId,
    pub expected_revision: u64,
    pub operation_id: String,
    pub objective: String,
    pub items: Vec<StoredWorkPlanItem>,
    pub updated_at_ms: i64,
}

/// WorkPlan 完整替换的权威结果；全部 Todo 完成时 Store 同事务清除当前计划。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkPlanMutationResult {
    /// 首次成功替换产生的计划，用于工具结果与 operation 幂等重放。
    pub plan: StoredWorkPlan,
    /// `true` 表示该替换已完成全部非空 Todo，当前计划已被自动清除。
    pub cleared: bool,
}

/// 以当前修订号 CAS 清除 WorkPlan。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkPlanClear {
    pub session_id: SessionId,
    pub expected_revision: u64,
}
