//! Session WorkPlan 领域状态、上下文投影与父 Agent 工具。

mod context;
mod tool;

use std::collections::BTreeSet;

use assistant_protocol::TodoItemId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{StoredTodoItemStatus, StoredWorkPlan, StoredWorkPlanItem, id};

#[cfg(test)]
pub(crate) use context::WORK_PLAN_CONTEXT_V1;
pub(crate) use context::inject_claimed_context;
pub(crate) use tool::{UpdatePlanTool, WorkPlanAuthorizationFacts};

const MAX_OBJECTIVE_BYTES: usize = 32 * 1024;
const MAX_ITEM_TEXT_BYTES: usize = 8 * 1024;
const MAX_ITEMS: usize = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TodoItemStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkPlanItem {
    pub(crate) id: TodoItemId,
    pub(crate) text: String,
    pub(crate) status: TodoItemStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkPlan {
    pub(crate) revision: u64,
    pub(crate) objective: String,
    pub(crate) items: Vec<WorkPlanItem>,
    pub(crate) updated_at_ms: i64,
}

pub(super) struct WorkPlanReplacementItem {
    pub(super) text: String,
    pub(super) status: TodoItemStatus,
}

#[derive(Debug)]
pub(super) enum WorkPlanBuildError {
    InvalidInput(String),
    Internal(&'static str),
}

impl WorkPlan {
    pub(super) fn replacement(
        current: Option<&Self>,
        objective: Option<String>,
        replacement_items: Vec<WorkPlanReplacementItem>,
        updated_at_ms: i64,
    ) -> Result<Self, WorkPlanBuildError> {
        let objective = objective
            .or_else(|| current.map(|plan| plan.objective.clone()))
            .ok_or_else(|| {
                WorkPlanBuildError::InvalidInput(
                    "objective is required when creating a work plan".to_owned(),
                )
            })?;
        let objective = validate_text("objective", objective, MAX_OBJECTIVE_BYTES)?;
        if replacement_items.len() > MAX_ITEMS {
            return Err(WorkPlanBuildError::InvalidInput(format!(
                "items must contain at most {MAX_ITEMS} entries"
            )));
        }
        let current_ids = current
            .into_iter()
            .flat_map(|plan| plan.items.iter().map(|item| item.id.clone()))
            .collect::<BTreeSet<_>>();
        let mut ids = BTreeSet::new();
        let mut items = Vec::with_capacity(replacement_items.len());
        for (position, item) in replacement_items.into_iter().enumerate() {
            let text = validate_text("item text", item.text, MAX_ITEM_TEXT_BYTES)?;
            // Todo ID 只服务 Runtime 持久化和 UI key。模型提交完整列表后，优先按文本、其次按
            // 原位置复用内部 ID；两者都不匹配才分配新 ID，模型无需维护内部标识。
            let id = reusable_todo_item_id(current, position, &text, &ids)
                .map_or_else(|| allocate_todo_item_id(&ids, &current_ids), Ok)?;
            let inserted = ids.insert(id.clone());
            debug_assert!(inserted, "reusable todo id must not be selected twice");
            items.push(WorkPlanItem {
                id,
                text,
                status: item.status,
            });
        }
        validate_in_progress(&items)?;
        Ok(Self {
            revision: current.map_or(1, |plan| plan.revision.saturating_add(1)),
            objective,
            items,
            updated_at_ms,
        })
    }
}

fn reusable_todo_item_id(
    current: Option<&WorkPlan>,
    position: usize,
    text: &str,
    used_ids: &BTreeSet<TodoItemId>,
) -> Option<TodoItemId> {
    let current = current?;
    current
        .items
        .iter()
        .find(|item| item.text == text && !used_ids.contains(&item.id))
        .or_else(|| {
            current
                .items
                .get(position)
                .filter(|item| !used_ids.contains(&item.id))
        })
        .or_else(|| {
            current
                .items
                .iter()
                .find(|item| !used_ids.contains(&item.id))
        })
        .map(|item| item.id.clone())
}

impl TryFrom<StoredWorkPlan> for WorkPlan {
    type Error = ();

    fn try_from(value: StoredWorkPlan) -> Result<Self, Self::Error> {
        if value.revision == 0
            || value.objective.trim().is_empty()
            || value.objective.len() > MAX_OBJECTIVE_BYTES
            || value.items.len() > MAX_ITEMS
        {
            return Err(());
        }
        let mut ids = BTreeSet::new();
        let mut items = Vec::with_capacity(value.items.len());
        for item in value.items {
            if !ids.insert(item.id.clone())
                || item.text.trim().is_empty()
                || item.text.len() > MAX_ITEM_TEXT_BYTES
            {
                return Err(());
            }
            items.push(WorkPlanItem {
                id: item.id,
                text: item.text,
                status: item.status.into(),
            });
        }
        if items
            .iter()
            .filter(|item| item.status == TodoItemStatus::InProgress)
            .count()
            > 1
        {
            return Err(());
        }
        Ok(Self {
            revision: value.revision,
            objective: value.objective,
            items,
            updated_at_ms: value.updated_at_ms,
        })
    }
}

impl From<&WorkPlanItem> for StoredWorkPlanItem {
    fn from(value: &WorkPlanItem) -> Self {
        Self {
            id: value.id.clone(),
            text: value.text.clone(),
            status: value.status.into(),
        }
    }
}

impl From<TodoItemStatus> for StoredTodoItemStatus {
    fn from(value: TodoItemStatus) -> Self {
        match value {
            TodoItemStatus::Pending => Self::Pending,
            TodoItemStatus::InProgress => Self::InProgress,
            TodoItemStatus::Completed => Self::Completed,
        }
    }
}

impl From<StoredTodoItemStatus> for TodoItemStatus {
    fn from(value: StoredTodoItemStatus) -> Self {
        match value {
            StoredTodoItemStatus::Pending => Self::Pending,
            StoredTodoItemStatus::InProgress => Self::InProgress,
            StoredTodoItemStatus::Completed => Self::Completed,
        }
    }
}

fn allocate_todo_item_id(
    proposed_ids: &BTreeSet<TodoItemId>,
    current_ids: &BTreeSet<TodoItemId>,
) -> Result<TodoItemId, WorkPlanBuildError> {
    for _ in 0..id::GENERATION_ATTEMPTS {
        let value = id::generate("todo")
            .map_err(|_| WorkPlanBuildError::Internal("todo item id source is unavailable"))?;
        let id = TodoItemId::new(value)
            .map_err(|_| WorkPlanBuildError::Internal("todo item id generator is invalid"))?;
        if !proposed_ids.contains(&id) && !current_ids.contains(&id) {
            return Ok(id);
        }
    }
    Err(WorkPlanBuildError::Internal(
        "todo item id could not be allocated",
    ))
}

fn validate_text(
    field: &'static str,
    value: String,
    max_bytes: usize,
) -> Result<String, WorkPlanBuildError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(WorkPlanBuildError::InvalidInput(format!(
            "{field} must not be blank"
        )));
    }
    if value.len() > max_bytes {
        return Err(WorkPlanBuildError::InvalidInput(format!(
            "{field} exceeds {max_bytes} UTF-8 bytes"
        )));
    }
    Ok(value)
}

fn validate_in_progress(items: &[WorkPlanItem]) -> Result<(), WorkPlanBuildError> {
    if items
        .iter()
        .filter(|item| item.status == TodoItemStatus::InProgress)
        .count()
        > 1
    {
        Err(WorkPlanBuildError::InvalidInput(
            "at most one item may be in_progress".to_owned(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_full_replacement_and_preserves_existing_ids() {
        let current = WorkPlan {
            revision: 1,
            objective: "ship".to_owned(),
            items: vec![WorkPlanItem {
                id: TodoItemId::new("todo-1").expect("id"),
                text: "first".to_owned(),
                status: TodoItemStatus::InProgress,
            }],
            updated_at_ms: 1,
        };
        let updated = WorkPlan::replacement(
            Some(&current),
            Some(" ship safely ".to_owned()),
            vec![WorkPlanReplacementItem {
                text: " done ".to_owned(),
                status: TodoItemStatus::Completed,
            }],
            2,
        )
        .expect("valid update");
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.objective, "ship safely");
        assert_eq!(updated.items[0].id.as_str(), "todo-1");
        assert_eq!(updated.items[0].text, "done");
    }

    #[test]
    fn rejects_multiple_active_items() {
        let items = vec![
            WorkPlanReplacementItem {
                text: "first".to_owned(),
                status: TodoItemStatus::InProgress,
            },
            WorkPlanReplacementItem {
                text: "second".to_owned(),
                status: TodoItemStatus::InProgress,
            },
        ];
        assert!(WorkPlan::replacement(None, Some("ship".to_owned()), items, 1).is_err());
    }

    #[test]
    fn keeps_objective_and_reconciles_runtime_ids_without_model_input() {
        let current = WorkPlan {
            revision: 4,
            objective: "ship".to_owned(),
            items: vec![
                WorkPlanItem {
                    id: TodoItemId::new("todo-1").expect("id"),
                    text: "first".to_owned(),
                    status: TodoItemStatus::InProgress,
                },
                WorkPlanItem {
                    id: TodoItemId::new("todo-2").expect("id"),
                    text: "second".to_owned(),
                    status: TodoItemStatus::Pending,
                },
            ],
            updated_at_ms: 1,
        };
        let updated = WorkPlan::replacement(
            Some(&current),
            None,
            vec![
                WorkPlanReplacementItem {
                    text: "second".to_owned(),
                    status: TodoItemStatus::Completed,
                },
                WorkPlanReplacementItem {
                    text: "first revised".to_owned(),
                    status: TodoItemStatus::InProgress,
                },
            ],
            2,
        )
        .expect("valid update without model ids");
        assert_eq!(updated.objective, "ship");
        assert_eq!(updated.items[0].id.as_str(), "todo-2");
        assert_eq!(updated.items[1].id.as_str(), "todo-1");
    }

    #[test]
    fn requires_objective_only_for_the_first_plan() {
        assert!(WorkPlan::replacement(None, None, Vec::new(), 1).is_err());
    }
}
