//! Provider 可见的稳定 `delegate_task` 工具契约。

use std::sync::Arc;

use agent_tools::{
    Tool, ToolContext, ToolError, ToolExecuteFuture, ToolExecutionMode, ToolResolution,
};
use agent_types::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DELEGATE_TASK_TOOL_NAME, DelegationAuthorizationFacts, ParentDelegationController};

const MAX_TITLE_BYTES: usize = 256;
const MAX_TASK_BYTES: usize = 32 * 1024;
const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_EXPECTED_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_APPROVAL_SUMMARY_CHARS: usize = 240;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DelegateTaskInput {
    title: String,
    task: String,
    context: Option<String>,
    expected_output: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DelegateTaskOutput {
    task_id: String,
    status: &'static str,
    result: String,
}

pub(crate) struct DelegateTaskTool {
    controller: Arc<ParentDelegationController>,
}

impl DelegateTaskTool {
    pub(crate) fn new(controller: Arc<ParentDelegationController>) -> Self {
        Self { controller }
    }
}

impl Tool for DelegateTaskTool {
    type Input = DelegateTaskInput;
    type ResolvedInput = DelegateTaskInput;
    type Output = DelegateTaskOutput;

    fn name(&self) -> ToolName {
        ToolName::new(DELEGATE_TASK_TOOL_NAME).expect("static tool name is valid")
    }

    fn description(&self) -> String {
        "Delegate one self-contained task to a non-recursive child agent and wait for its final result. The child shares the current model, workspace view, permissions, and base tools, but does not receive the parent conversation or delegate_task itself.".to_owned()
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::ParallelEligible
    }

    fn resolve(
        &self,
        mut input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input.title = validated_required("title", input.title, MAX_TITLE_BYTES)?;
        input.task = validated_required("task", input.task, MAX_TASK_BYTES)?;
        input.context = validated_optional("context", input.context, MAX_CONTEXT_BYTES)?;
        input.expected_output = validated_optional(
            "expected_output",
            input.expected_output,
            MAX_EXPECTED_OUTPUT_BYTES,
        )?;
        let semantic_arguments = serde_json::to_value(&input)
            .map_err(|_| ToolError::invalid_input("delegate_task input could not be resolved"))?;
        let facts = DelegationAuthorizationFacts {
            title: input.title.clone(),
            task_summary: approval_summary(&input.task),
        };
        // 专用 facts 只用于审批展示；权限 matcher 仍把它视为 General delegate_task。
        Ok(ToolResolution::with_facts(input, facts, semantic_arguments))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let call_id = context.call_id().cloned().ok_or_else(|| {
                ToolError::execution("delegate_task call identity is unavailable")
            })?;
            self.controller
                .execute(input, call_id, context.cancellation)
                .await
        })
    }
}

impl DelegateTaskInput {
    pub(super) fn title(&self) -> &str {
        &self.title
    }

    pub(super) fn task(&self) -> &str {
        &self.task
    }

    pub(super) fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub(super) fn expected_output(&self) -> Option<&str> {
        self.expected_output.as_deref()
    }
}

impl DelegateTaskOutput {
    pub(super) fn completed(task_id: &assistant_protocol::ChildTaskId, result: String) -> Self {
        Self {
            task_id: task_id.as_str().to_owned(),
            status: "completed",
            result,
        }
    }
}

fn validated_required(
    field: &'static str,
    value: String,
    max_bytes: usize,
) -> Result<String, ToolError> {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(ToolError::invalid_input(format!(
            "{field} must not be blank"
        )));
    }
    if value.len() > max_bytes {
        return Err(ToolError::invalid_input(format!(
            "{field} exceeds {max_bytes} UTF-8 bytes"
        )));
    }
    Ok(value)
}

fn validated_optional(
    field: &'static str,
    value: Option<String>,
    max_bytes: usize,
) -> Result<Option<String>, ToolError> {
    value
        .map(|value| {
            if value.len() > max_bytes {
                Err(ToolError::invalid_input(format!(
                    "{field} exceeds {max_bytes} UTF-8 bytes"
                )))
            } else {
                Ok(value)
            }
        })
        .transpose()
}

fn approval_summary(task: &str) -> String {
    let mut chars = task.chars();
    let summary = chars
        .by_ref()
        .take(MAX_APPROVAL_SUMMARY_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{summary}…")
    } else {
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_required_and_bounded_fields() {
        assert_eq!(
            validated_required("title", "  inspect  ".to_owned(), 16).expect("valid"),
            "inspect"
        );
        assert!(validated_required("title", "  ".to_owned(), 16).is_err());
        assert!(validated_optional("context", Some("abc".to_owned()), 2).is_err());
    }

    #[test]
    fn rejects_model_control_fields_and_bounds_approval_summary() {
        let error = serde_json::from_value::<DelegateTaskInput>(serde_json::json!({
            "title": "inspect",
            "task": "read files",
            "model": "other"
        }))
        .expect_err("unknown model control must be rejected");
        assert!(error.to_string().contains("unknown field"));

        let summary = approval_summary(&"你".repeat(MAX_APPROVAL_SUMMARY_CHARS + 1));
        assert_eq!(summary.chars().count(), MAX_APPROVAL_SUMMARY_CHARS + 1);
        assert!(summary.ends_with('…'));
    }
}
