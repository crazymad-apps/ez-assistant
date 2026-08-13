//! Core 子执行事件到公共 child payload 的无状态转换。

use agent_core::{AgentEvent, ToolCompletionStatus};
use agent_tools::ToolOutputChannel as AgentOutputChannel;
use assistant_protocol::{
    ChildTaskEvent, PartId, TokenUsageSnapshot, ToolActivityStatus, ToolCallId, ToolOutputChannel,
};

pub(super) fn project(event: AgentEvent) -> Option<ChildTaskEvent> {
    match event {
        AgentEvent::TextDelta { id, delta } => Some(ChildTaskEvent::TextDelta {
            part_id: PartId::new(id.as_str()).ok()?,
            delta,
        }),
        AgentEvent::ReasoningDelta { id, delta } => Some(ChildTaskEvent::ReasoningDelta {
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
        AgentEvent::ToolProposed { call } => Some(ChildTaskEvent::ToolProposed {
            call_id: ToolCallId::new(call.id.as_str()).ok()?,
            tool_name: call.name.as_str().to_owned(),
        }),
        AgentEvent::ToolStarted { call_id } => Some(ChildTaskEvent::ToolStarted {
            call_id: ToolCallId::new(call_id.as_str()).ok()?,
        }),
        AgentEvent::ToolOutput {
            call_id,
            channel,
            chunk,
        } => Some(ChildTaskEvent::ToolOutput {
            call_id: ToolCallId::new(call_id.as_str()).ok()?,
            channel: match channel {
                AgentOutputChannel::Stdout => ToolOutputChannel::Stdout,
                AgentOutputChannel::Stderr => ToolOutputChannel::Stderr,
            },
            chunk,
        }),
        AgentEvent::ToolCompleted { call_id, status } => Some(ChildTaskEvent::ToolCompleted {
            call_id: ToolCallId::new(call_id.as_str()).ok()?,
            status: match status {
                ToolCompletionStatus::Success => ToolActivityStatus::Completed,
                ToolCompletionStatus::Failed => ToolActivityStatus::Failed,
            },
        }),
        AgentEvent::ExecutionStarted
        | AgentEvent::StepStarted { .. }
        | AgentEvent::GuardrailTriggered { .. }
        | AgentEvent::ExecutionCompleted { .. }
        | AgentEvent::ExecutionFailed { .. }
        | AgentEvent::ExecutionCancelled { .. }
        | AgentEvent::ExecutionCompactionRequired { .. } => None,
    }
}
