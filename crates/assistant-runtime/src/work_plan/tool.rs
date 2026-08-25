//! `update_plan` 工具适配与持久化协调。

use std::sync::Arc;

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::{ToolCallId, ToolName};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{TodoItemStatus, WorkPlan, WorkPlanBuildError, WorkPlanReplacementItem};
use crate::{
    RuntimeStore, StoreErrorKind, StoredWorkPlanItem, WorkPlanMutation,
    observation::ObservationCoordinator, runtime::now_ms, session::SessionController,
};

const UPDATE_PLAN_TOOL_NAME: &str = "update_plan";

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdatePlanInput {
    /// 模型侧每次都提交完整目标；Option 只用于兼容已进入历史的缺省调用。
    #[schemars(required)]
    objective: Option<String>,
    items: Vec<UpdatePlanItemInput>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdatePlanItemInput {
    /// 兼容已进入历史 Conversation 的旧工具定义；新 schema 不再把 Runtime 内部 ID 暴露给模型。
    #[schemars(skip)]
    #[serde(default, rename = "id", skip_serializing)]
    legacy_id: Option<String>,
    text: String,
    status: TodoItemStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct UpdatePlanOutput {
    revision: u64,
    objective: String,
    items: Vec<UpdatePlanOutputItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct UpdatePlanOutputItem {
    text: String,
    status: TodoItemStatus,
}

/// 只有 Runtime 派生的 `update_plan` 能产生此 facts；Host 基础工具无法伪造私有类型。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkPlanAuthorizationFacts;

pub(crate) struct UpdatePlanTool {
    session: Arc<SessionController>,
    store: Arc<dyn RuntimeStore>,
    events: ObservationCoordinator,
}

impl UpdatePlanTool {
    pub(crate) fn new(
        session: Arc<SessionController>,
        store: Arc<dyn RuntimeStore>,
        events: ObservationCoordinator,
    ) -> Self {
        Self {
            session,
            store,
            events,
        }
    }
}

impl Tool for UpdatePlanTool {
    type Input = UpdatePlanInput;
    type ResolvedInput = UpdatePlanInput;
    type Output = UpdatePlanOutput;

    fn name(&self) -> ToolName {
        ToolName::new(UPDATE_PLAN_TOOL_NAME).expect("static tool name is valid")
    }

    fn description(&self) -> String {
        "Replace the current session work plan with the complete objective and ordered item list. Always provide objective; when only updating items, repeat the current objective unchanged. Item ids are Runtime-managed and must not be supplied. Keep at most one item in_progress. This records progress but does not enable automatic continuation."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let semantic_arguments = serde_json::to_value(&input)
            .map_err(|_| ToolError::invalid_input("update_plan input could not be resolved"))?;
        Ok(ToolResolution::with_facts(
            input,
            WorkPlanAuthorizationFacts,
            semantic_arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let call_id = context
                .call_id()
                .cloned()
                .ok_or_else(|| ToolError::execution("update_plan call identity is unavailable"))?;
            execute_update(self, input, call_id).await
        })
    }
}

async fn execute_update(
    tool: &UpdatePlanTool,
    input: UpdatePlanInput,
    call_id: ToolCallId,
) -> Result<UpdatePlanOutput, ToolError> {
    let _mutation = tool.session.mutation().await;
    tool.session
        .ensure_healthy()
        .map_err(|_| ToolError::execution("session state is unavailable"))?;
    tool.session
        .ensure_active()
        .map_err(|_| ToolError::execution("session is archived"))?;
    let current = tool
        .session
        .lock_state()
        .map_err(|_| ToolError::execution("session state is unavailable"))?
        .work_plan
        .clone();
    let expected_revision = current.as_ref().map_or(0, |plan| plan.revision);
    let updated_at_ms =
        now_ms().map_err(|_| ToolError::execution("system clock is unavailable"))?;
    let replacement_items = input
        .items
        .into_iter()
        .map(|item| {
            let UpdatePlanItemInput {
                legacy_id,
                text,
                status,
            } = item;
            // 旧 ID 只为兼容反序列化而消费，不参与 resolved facts、身份调和或持久化。
            drop(legacy_id);
            WorkPlanReplacementItem { text, status }
        })
        .collect();
    let proposed = WorkPlan::replacement(
        current.as_ref(),
        input.objective,
        replacement_items,
        updated_at_ms,
    )
    .map_err(|error| match error {
        WorkPlanBuildError::InvalidInput(message) => ToolError::invalid_input(message),
        WorkPlanBuildError::Internal(message) => ToolError::execution(message),
    })?;
    let stored_result = tool
        .store
        .mutate_work_plan(WorkPlanMutation {
            session_id: tool.session.id().clone(),
            expected_revision,
            operation_id: call_id.as_str().to_owned(),
            objective: proposed.objective.clone(),
            items: proposed
                .items
                .iter()
                .map(StoredWorkPlanItem::from)
                .collect(),
            updated_at_ms,
        })
        .await
        .map_err(|error| match error.kind() {
            StoreErrorKind::Conflict => {
                ToolError::execution("work plan changed; reload the latest plan and retry")
            }
            _ => ToolError::execution("work plan could not be persisted"),
        })?;
    let cleared = stored_result.cleared;
    let persisted = WorkPlan::try_from(stored_result.plan)
        .map_err(|_| ToolError::execution("persisted work plan is invalid"))?;
    tool.session
        .lock_state()
        .map_err(|_| ToolError::execution("session state is unavailable"))?
        .work_plan = (!cleared).then(|| persisted.clone());
    // SSE 只广播失效事实；计划正文仍由 Desktop 重新获取 SessionView。
    let _ = tool
        .events
        .send(assistant_protocol::RuntimeEvent::WorkPlanChanged {
            session_id: tool.session.id().clone(),
            revision: persisted.revision,
        });
    Ok(UpdatePlanOutput::from(&persisted))
}

impl From<&WorkPlan> for UpdatePlanOutput {
    fn from(value: &WorkPlan) -> Self {
        Self {
            revision: value.revision,
            objective: value.objective.clone(),
            items: value
                .items
                .iter()
                .map(|item| UpdatePlanOutputItem {
                    text: item.text.clone(),
                    status: item.status,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use assistant_protocol::TodoItemId;
    use serde_json::json;

    use super::*;
    use crate::work_plan::WorkPlanItem;

    #[test]
    fn model_schema_requires_objective_and_omits_runtime_ids() {
        let schema = serde_json::to_value(schemars::schema_for!(UpdatePlanInput))
            .expect("encode update plan schema");
        let required = schema["required"].as_array().expect("root required fields");
        assert!(required.iter().any(|field| field == "objective"));
        let encoded = serde_json::to_string(&schema).expect("schema json");
        assert!(!encoded.contains("\"id\":"));

        // Serde 仍接受已进入历史的旧调用；只有新的模型工具定义要求完整提交。
        let legacy = serde_json::from_value::<UpdatePlanInput>(json!({
            "items": [{
                "id": "todo-no-longer-current",
                "text": "verify",
                "status": "in_progress"
            }]
        }))
        .expect("legacy id remains accepted");
        assert!(legacy.objective.is_none());
        assert_eq!(
            legacy.items[0].legacy_id.as_deref(),
            Some("todo-no-longer-current")
        );
        let normalized = serde_json::to_value(&legacy).expect("normalize legacy input");
        assert!(normalized["items"][0].get("id").is_none());
    }

    #[test]
    fn model_output_does_not_expose_runtime_ids() {
        let plan = WorkPlan {
            revision: 2,
            objective: "ship".to_owned(),
            items: vec![WorkPlanItem {
                id: TodoItemId::new("todo-internal").expect("todo id"),
                text: "verify".to_owned(),
                status: TodoItemStatus::InProgress,
            }],
            updated_at_ms: 1,
        };
        let output =
            serde_json::to_value(UpdatePlanOutput::from(&plan)).expect("encode update plan output");
        assert_eq!(output["items"][0]["text"], "verify");
        assert!(output["items"][0].get("id").is_none());
    }
}
