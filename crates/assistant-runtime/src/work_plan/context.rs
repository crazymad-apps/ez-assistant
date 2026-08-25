//! WorkPlan 到模型隐藏上下文的投影。

use agent_types::UserMessage;
use serde::Serialize;

use super::{TodoItemStatus, WorkPlan};
use crate::{
    RuntimeError, RuntimeResult,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
};

pub(crate) const WORK_PLAN_CONTEXT_V1: &str = "WORK_PLAN_CONTEXT_V1";

/// 将领取时冻结的 WorkPlan 注入本条消息；不复制排队消息，也不改写历史消息。
pub(crate) fn inject_claimed_context(
    mut message: Option<UserMessage>,
    plan: Option<&WorkPlan>,
) -> RuntimeResult<Option<UserMessage>> {
    let Some(plan) = plan else {
        return Ok(message);
    };
    let Some(claimed_message) = message.as_mut() else {
        return Ok(None);
    };
    let text = context_text(plan).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "work plan context projection",
    })?;
    InternalBoundaryCoordinator::append(
        claimed_message,
        InternalBoundaryRequest {
            source: InternalBoundarySource::WorkPlan,
            retention_key: Some("work_plan".to_owned()),
            text,
        },
    )?;
    Ok(message)
}

fn context_text(plan: &WorkPlan) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct Context<'a> {
        revision: u64,
        objective: &'a str,
        items: Vec<ContextItem<'a>>,
    }
    #[derive(Serialize)]
    struct ContextItem<'a> {
        text: &'a str,
        status: TodoItemStatus,
    }
    let context = Context {
        revision: plan.revision,
        objective: &plan.objective,
        items: plan
            .items
            .iter()
            .map(|item| ContextItem {
                text: &item.text,
                status: item.status,
            })
            .collect(),
    };
    serde_json::to_string(&context).map(|json| {
        format!(
            "{WORK_PLAN_CONTEXT_V1}\nThis is the Runtime-maintained session work plan. Its items intentionally have no ids. Use update_plan with the complete objective and ordered item list to record material changes. Always provide objective; when only updating items, repeat the current objective unchanged. The plan does not enable automatic continuation.\n{json}"
        )
    })
}

#[cfg(test)]
mod tests {
    use assistant_protocol::TodoItemId;

    use super::*;
    use crate::work_plan::WorkPlanItem;

    #[test]
    fn context_is_versioned_without_exposing_runtime_item_ids() {
        let plan = WorkPlan {
            revision: 3,
            objective: "ship".to_owned(),
            items: vec![WorkPlanItem {
                id: TodoItemId::new("todo-1").expect("id"),
                text: "verify".to_owned(),
                status: TodoItemStatus::Pending,
            }],
            updated_at_ms: 1,
        };
        let context = context_text(&plan).expect("context");
        assert!(context.starts_with(WORK_PLAN_CONTEXT_V1));
        assert!(context.contains("verify"));
        assert!(!context.contains("todo-1"));
    }
}
