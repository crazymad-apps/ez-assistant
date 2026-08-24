//! `update_goal` 工具与当前 Run 的一次性终止信号 latch。

use std::sync::{Arc, Mutex};

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::ToolName;
use assistant_protocol::{GoalId, RunId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const UPDATE_GOAL_TOOL_NAME: &str = "update_goal";
const MAX_SUMMARY_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalRunBinding {
    pub(crate) goal_id: GoalId,
    pub(crate) generation: u64,
    pub(crate) run_id: RunId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoalAgentStatus {
    Complete,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalRunSignal {
    pub(crate) binding: GoalRunBinding,
    pub(crate) status: GoalAgentStatus,
    pub(crate) summary: String,
}

pub(crate) struct GoalRunSignalLatch {
    binding: GoalRunBinding,
    signal: Mutex<Option<GoalRunSignal>>,
}

impl GoalRunSignalLatch {
    pub(crate) fn new(binding: GoalRunBinding) -> Self {
        Self {
            binding,
            signal: Mutex::new(None),
        }
    }

    pub(crate) fn signal(&self) -> Option<GoalRunSignal> {
        self.signal.lock().ok().and_then(|signal| signal.clone())
    }

    pub(crate) fn has_signal(&self) -> bool {
        self.signal().is_some()
    }

    pub(crate) fn record(
        &self,
        status: GoalAgentStatus,
        summary: String,
    ) -> Result<GoalRunSignal, ToolError> {
        let mut current = self
            .signal
            .lock()
            .map_err(|_| ToolError::execution("goal signal latch is unavailable"))?;
        if current.is_some() {
            return Err(ToolError::execution(
                "this Run already reported a Goal terminal signal",
            ));
        }
        let signal = GoalRunSignal {
            binding: self.binding.clone(),
            status,
            summary,
        };
        *current = Some(signal.clone());
        Ok(signal)
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateGoalInput {
    status: GoalAgentStatus,
    summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct UpdateGoalOutput {
    status: GoalAgentStatus,
    summary: String,
    applies_after_run_settlement: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalSignalAuthorizationFacts;

pub(crate) struct UpdateGoalTool {
    latch: Option<Arc<GoalRunSignalLatch>>,
}

impl UpdateGoalTool {
    pub(crate) fn new(latch: Option<Arc<GoalRunSignalLatch>>) -> Self {
        Self { latch }
    }
}

impl Tool for UpdateGoalTool {
    type Input = UpdateGoalInput;
    type ResolvedInput = UpdateGoalInput;
    type Output = UpdateGoalOutput;

    fn name(&self) -> ToolName {
        ToolName::new(UPDATE_GOAL_TOOL_NAME).expect("static tool name is valid")
    }

    fn description(&self) -> String {
        "Report that the current Goal is complete or blocked. Call this alone in its tool batch. The signal only takes effect after the current Run is reliably completed."
            .to_owned()
    }

    fn resolve(
        &self,
        mut input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input.summary = input.summary.trim().to_owned();
        if input.summary.is_empty() {
            return Err(ToolError::invalid_input("summary must not be blank"));
        }
        if input.summary.len() > MAX_SUMMARY_BYTES {
            return Err(ToolError::invalid_input(format!(
                "summary exceeds {MAX_SUMMARY_BYTES} UTF-8 bytes"
            )));
        }
        let semantic_arguments = serde_json::to_value(&input)
            .map_err(|_| ToolError::invalid_input("update_goal input could not be resolved"))?;
        Ok(ToolResolution::with_facts(
            input,
            GoalSignalAuthorizationFacts,
            semantic_arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        _context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let latch = self
                .latch
                .as_ref()
                .ok_or_else(|| ToolError::execution("there is no active Goal bound to this Run"))?;
            let signal = latch.record(input.status, input.summary)?;
            Ok(UpdateGoalOutput {
                status: signal.status,
                summary: signal.summary,
                applies_after_run_settlement: true,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latch() -> Arc<GoalRunSignalLatch> {
        Arc::new(GoalRunSignalLatch::new(GoalRunBinding {
            goal_id: GoalId::new("goal-1").expect("goal id"),
            generation: 7,
            run_id: RunId::new("run-1").expect("run id"),
        }))
    }

    #[tokio::test]
    async fn records_exactly_one_signal_without_mutating_goal_state() {
        let latch = latch();
        let tool = UpdateGoalTool::new(Some(latch.clone()));
        let input = tool
            .resolve(UpdateGoalInput {
                status: GoalAgentStatus::Complete,
                summary: " objective completed ".to_owned(),
            })
            .expect("resolve")
            .into_input();
        let output = tool
            .execute(input, ToolContext::default())
            .await
            .expect("record signal");
        assert!(output.applies_after_run_settlement);
        assert_eq!(latch.signal().expect("signal").binding.generation, 7);

        let duplicate = tool
            .resolve(UpdateGoalInput {
                status: GoalAgentStatus::Blocked,
                summary: "blocked".to_owned(),
            })
            .expect("resolve")
            .into_input();
        assert!(
            tool.execute(duplicate, ToolContext::default())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn ordinary_run_receives_a_controlled_error() {
        let tool = UpdateGoalTool::new(None);
        let input = tool
            .resolve(UpdateGoalInput {
                status: GoalAgentStatus::Blocked,
                summary: "need user input".to_owned(),
            })
            .expect("resolve")
            .into_input();
        assert!(tool.execute(input, ToolContext::default()).await.is_err());
    }
}
