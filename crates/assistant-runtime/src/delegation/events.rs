//! Core 子执行事件到公共 child payload 的无状态转换。

use agent_core::{AgentEvent, ToolCompletionStatus};
use agent_tools::ToolOutputChannel as AgentOutputChannel;
use assistant_protocol::{
    ChildTaskEvent, PartId, TokenUsageSnapshot, ToolActivityStatus, ToolCallId, ToolOutputChannel,
};

pub(super) fn project(event: AgentEvent) -> Option<ChildTaskEvent> {
    match event {
        AgentEvent::TextDelta { step, id, delta } => Some(ChildTaskEvent::TextDelta {
            step,
            part_id: PartId::new(id.as_str()).ok()?,
            delta,
        }),
        AgentEvent::ReasoningDelta { step, id, delta } => Some(ChildTaskEvent::ReasoningDelta {
            step,
            part_id: PartId::new(id.as_str()).ok()?,
            delta,
        }),
        AgentEvent::UsageUpdated { step, usage } => Some(ChildTaskEvent::UsageUpdated {
            step,
            usage: TokenUsageSnapshot {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                cached_input_tokens: usage.cached_input_tokens,
            },
        }),
        AgentEvent::ToolProposed { step, call } => Some(ChildTaskEvent::ToolProposed {
            step,
            call_id: ToolCallId::new(call.id.as_str()).ok()?,
            tool_name: call.name.as_str().to_owned(),
        }),
        AgentEvent::ToolStarted { step, call_id } => Some(ChildTaskEvent::ToolStarted {
            step,
            call_id: ToolCallId::new(call_id.as_str()).ok()?,
        }),
        AgentEvent::ToolOutput {
            step,
            call_id,
            channel,
            chunk,
        } => Some(ChildTaskEvent::ToolOutput {
            step,
            call_id: ToolCallId::new(call_id.as_str()).ok()?,
            channel: match channel {
                AgentOutputChannel::Stdout => ToolOutputChannel::Stdout,
                AgentOutputChannel::Stderr => ToolOutputChannel::Stderr,
            },
            chunk,
        }),
        AgentEvent::ToolCompleted {
            step,
            call_id,
            status,
        } => Some(ChildTaskEvent::ToolCompleted {
            step,
            call_id: ToolCallId::new(call_id.as_str()).ok()?,
            status: match status {
                ToolCompletionStatus::Success => ToolActivityStatus::Completed,
                ToolCompletionStatus::Failed => ToolActivityStatus::Failed,
            },
        }),
        AgentEvent::StepStarted { step } => Some(ChildTaskEvent::StepStarted { step }),
        AgentEvent::ExecutionStarted
        | AgentEvent::GuardrailTriggered { .. }
        | AgentEvent::ExecutionCompleted { .. }
        | AgentEvent::ExecutionFailed { .. }
        | AgentEvent::ExecutionCancelled { .. }
        | AgentEvent::ExecutionCompactionRequired { .. }
        | AgentEvent::ExecutionContinuationRequired { .. } => None,
    }
}
