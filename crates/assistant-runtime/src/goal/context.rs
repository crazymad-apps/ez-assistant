//! Goal 首次消息的版本化 Runtime 注入。

use agent_types::UserMessage;
use assistant_protocol::GoalId;

use crate::{
    RuntimeError, RuntimeResult,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
};

use super::GoalControl;

pub(crate) const GOAL_START_INJECTION_V1: &str = "GOAL_START_INJECTION_V1";
pub(crate) const GOAL_CONTINUATION_V1: &str = "GOAL_CONTINUATION_V1";
pub(crate) const GOAL_RESUME_INJECTION_V1: &str = "GOAL_RESUME_INJECTION_V1";

/// 在首次可见用户消息中冻结 Goal 执行说明；不复制或改写用户正文。
pub(crate) fn inject_start_context(
    message: &mut UserMessage,
    goal_id: &GoalId,
) -> RuntimeResult<()> {
    let escaped_goal_id = escape_xml(goal_id.as_str());
    let text = format!(
        "{GOAL_START_INJECTION_V1}\n<goal_context version=\"1\"><goal_id>{escaped_goal_id}</goal_id><generation>1</generation><turn>1</turn><instructions>This user message starts an automatically continued Goal. Work toward its complete objective across Runs. A normal final answer does not end the Goal. When the objective is complete or progress requires user input, call update_goal as the only tool call in that assistant turn and then provide the final user-facing response.</instructions></goal_context>"
    );
    InternalBoundaryCoordinator::insert_before(
        message,
        InternalBoundarySource::SkillActivation,
        InternalBoundaryRequest {
            source: InternalBoundarySource::GoalStart,
            text,
        },
    )?;
    Ok(())
}

/// 在恢复用户消息中附加原 Goal 的冻结上下文；用户正文仍作为可见会话内容保留。
pub(crate) fn inject_resume_context(
    message: &mut UserMessage,
    goal: &GoalControl,
) -> RuntimeResult<()> {
    #[derive(serde::Serialize)]
    struct ResumeContext<'a> {
        goal_id: &'a str,
        generation: u64,
        turn: u32,
        objective: Vec<crate::StoredGoalObjectivePart>,
    }
    let json = serde_json::to_string(&ResumeContext {
        goal_id: goal.id.as_str(),
        generation: goal.generation,
        turn: goal.turn,
        objective: goal.stored_objective_parts(),
    })
    .map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "Goal resume context projection",
    })?;
    InternalBoundaryCoordinator::insert_before(
        message,
        InternalBoundarySource::SkillActivation,
        InternalBoundaryRequest {
            source: InternalBoundarySource::GoalResume,
            text: format!(
                "{GOAL_RESUME_INJECTION_V1}\nResume autonomous work toward the frozen Goal objective. Treat this visible user message as additional guidance, not as a replacement objective. A normal final answer does not end the Goal.\n{json}"
            ),
        },
    )?;
    Ok(())
}

/// 创建只含冻结 Injected Part 的隐藏 Runtime continuation。
pub(crate) fn create_continuation_message(
    goal: &GoalControl,
    previous_run_status: &str,
) -> RuntimeResult<UserMessage> {
    #[derive(serde::Serialize)]
    struct ContinuationContext<'a> {
        goal_id: &'a str,
        generation: u64,
        turn: u32,
        remaining_runs: u32,
        remaining_total_tokens: u64,
        usage_complete: bool,
        previous_run_status: &'a str,
        objective: Vec<crate::StoredGoalObjectivePart>,
    }
    let context = ContinuationContext {
        goal_id: goal.id.as_str(),
        generation: goal.generation,
        turn: goal.turn,
        remaining_runs: goal.budget.max_runs.saturating_sub(goal.budget.used_runs),
        remaining_total_tokens: goal
            .budget
            .max_total_tokens
            .saturating_sub(goal.budget.used_total_tokens),
        usage_complete: goal.budget.usage_complete,
        previous_run_status,
        objective: goal.stored_objective_parts(),
    };
    let json =
        serde_json::to_string(&context).map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "Goal continuation context projection",
        })?;
    let text = format!(
        "{GOAL_CONTINUATION_V1}\nContinue working autonomously toward the frozen Goal objective. A normal final answer does not end the Goal. Call update_goal alone only when the objective is complete or progress requires user input.\n{json}"
    );
    InternalBoundaryCoordinator::hidden_message(InternalBoundaryRequest {
        source: InternalBoundarySource::GoalContinuation,
        text,
    })
    .map(|(message, _)| message)
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use agent_types::{MessageId, TranscriptVisibility, UserMessageOrigin, UserPart};

    use super::*;

    #[test]
    fn start_context_is_versioned_and_escapes_goal_id() {
        let mut message = UserMessage {
            id: MessageId::new("message-1").expect("message id"),
            origin: UserMessageOrigin::User,
            transcript_visibility: TranscriptVisibility::Visible,
            parts: Vec::new(),
        };
        inject_start_context(
            &mut message,
            &GoalId::new("goal<&").expect("opaque goal id"),
        )
        .expect("inject");
        let UserPart::InternalContext(part) = message.parts.last().expect("internal part") else {
            panic!("expected internal context part");
        };
        assert!(part.text.starts_with(GOAL_START_INJECTION_V1));
        assert!(part.text.contains("goal&lt;&amp;"));
        assert!(!part.text.contains("goal<&"));
    }
}
