//! 只读 Timeline：默认只展示类型、关联和长度，`--full` 才展示高敏 payload。

use std::fmt::Write as _;

use agent_core::AgentEvent;
use agent_model::{ModelAttemptEvent, ModelEvent};
use agent_openai_compatible::ProviderWireEvent;

use crate::trace::{
    DemoHostEvent, LoadedTrace, ModelCallEvent, NativeTracePayload, TraceCompleteness, TraceRecord,
};

pub(crate) fn render_timeline(trace: &LoadedTrace, full: bool) -> String {
    let mut output = String::new();
    let status = match trace.completeness {
        TraceCompleteness::Complete => "complete",
        TraceCompleteness::Incomplete => "incomplete",
    };
    let _ = writeln!(
        output,
        "trace={} status={} format={} records={}",
        trace.started.trace_id,
        status,
        trace.started.format_version,
        trace.records.len()
    );
    let _ = writeln!(
        output,
        "adapter={} adapter_version={} protocol_adapter={} provider={} protocol={} model={} context_window={}",
        trace.started.provider.adapter,
        trace.started.provider.adapter_version,
        trace.started.provider.protocol_adapter,
        trace.started.provider.provider_id,
        trace.started.provider.protocol,
        trace.started.provider.model,
        trace.started.provider.context_window_tokens
    );
    for record in &trace.records {
        let correlation = record.correlation_id.as_deref().unwrap_or("-");
        let attempt = record
            .attempt
            .map_or_else(|| "-".to_owned(), |attempt| attempt.to_string());
        let detail = if full {
            serde_json::to_string(&record.payload)
                .unwrap_or_else(|_| "<payload serialization failed>".into())
        } else {
            summarize(record)
        };
        let _ = writeln!(
            output,
            "#{:06} t={} layer={} correlation={} attempt={} {}",
            record.sequence,
            record.observed_at_ms,
            record.layer.as_str(),
            correlation,
            attempt,
            detail
        );
    }
    output
}

fn summarize(record: &TraceRecord) -> String {
    match &record.payload {
        NativeTracePayload::ModelRequest(request) => format!(
            "model_request system_parts={} messages={} tools={}",
            request.system.parts().len(),
            request.conversation.messages.len(),
            request.tools.len()
        ),
        NativeTracePayload::ModelCall(event) => match event {
            ModelCallEvent::StreamEstablished => "model_call stream_established".into(),
            ModelCallEvent::EstablishmentFailed { .. } => "model_call establishment_failed".into(),
        },
        NativeTracePayload::ModelEvent(event) => summarize_model_event(event),
        NativeTracePayload::ModelAttempt(event) => summarize_attempt_event(event),
        NativeTracePayload::ProviderWire(event) => summarize_wire_event(event),
        NativeTracePayload::Agent(event) => summarize_agent_event(event),
        NativeTracePayload::Host(event) => summarize_host_event(event),
    }
}

fn summarize_model_event(event: &ModelEvent) -> String {
    match event {
        ModelEvent::TurnStarted { .. } => "model_event turn_started".into(),
        ModelEvent::ReasoningStarted { .. } => "model_event reasoning_started".into(),
        ModelEvent::ReasoningDelta { delta, .. } => {
            format!("model_event reasoning_delta bytes={}", delta.len())
        }
        ModelEvent::ReasoningFinished { .. } => "model_event reasoning_finished".into(),
        ModelEvent::TextStarted { .. } => "model_event text_started".into(),
        ModelEvent::TextDelta { delta, .. } => {
            format!("model_event text_delta bytes={}", delta.len())
        }
        ModelEvent::TextFinished { .. } => "model_event text_finished".into(),
        ModelEvent::ToolCallStarted { .. } => "model_event tool_call_started".into(),
        ModelEvent::ToolCallDelta {
            arguments_delta, ..
        } => format!(
            "model_event tool_call_delta bytes={}",
            arguments_delta.len()
        ),
        ModelEvent::ToolCallFinished { .. } => "model_event tool_call_finished".into(),
        ModelEvent::UsageUpdated { .. } => "model_event usage_updated".into(),
        ModelEvent::TurnFinished { .. } => "model_event turn_finished".into(),
        ModelEvent::TurnFailed { .. } => "model_event turn_failed".into(),
    }
}

fn summarize_attempt_event(event: &ModelAttemptEvent) -> String {
    match event {
        ModelAttemptEvent::Started { attempt, .. } => {
            format!("model_attempt started attempt={attempt}")
        }
        ModelAttemptEvent::EstablishmentFailed {
            attempt,
            retry_reason,
            will_retry,
            ..
        } => format!(
            "model_attempt establishment_failed attempt={attempt} reason={retry_reason:?} will_retry={will_retry}"
        ),
        ModelAttemptEvent::RetryScheduled {
            next_attempt,
            delay_ms,
            ..
        } => {
            format!("model_attempt retry_scheduled next_attempt={next_attempt} delay_ms={delay_ms}")
        }
        ModelAttemptEvent::StreamEstablished { attempt, .. } => {
            format!("model_attempt stream_established attempt={attempt}")
        }
    }
}

fn summarize_wire_event(event: &ProviderWireEvent) -> String {
    match event {
        ProviderWireEvent::Request { request, .. } => format!(
            "provider_wire request method={} headers={} body_bytes={}",
            request.method,
            request.headers.len(),
            request.body.len()
        ),
        ProviderWireEvent::ResponseStarted {
            status, headers, ..
        } => format!(
            "provider_wire response_started status={} headers={}",
            status,
            headers.len()
        ),
        ProviderWireEvent::ResponseChunk { bytes, .. } => {
            format!("provider_wire response_chunk bytes={}", bytes.len())
        }
        ProviderWireEvent::ResponseFailed { .. } => "provider_wire response_failed".into(),
        ProviderWireEvent::ResponseFinished { .. } => "provider_wire response_finished".into(),
    }
}

fn summarize_agent_event(event: &AgentEvent) -> String {
    match event {
        AgentEvent::ExecutionStarted => "agent_event execution_started".into(),
        AgentEvent::StepStarted { step } => format!("agent_event step_started step={step}"),
        AgentEvent::UsageUpdated { step, .. } => {
            format!("agent_event usage_updated step={step}")
        }
        AgentEvent::TextDelta { delta, .. } => {
            format!("agent_event text_delta bytes={}", delta.len())
        }
        AgentEvent::ReasoningDelta { delta, .. } => {
            format!("agent_event reasoning_delta bytes={}", delta.len())
        }
        AgentEvent::ToolProposed { .. } => "agent_event tool_proposed".into(),
        AgentEvent::ToolStarted { .. } => "agent_event tool_started".into(),
        AgentEvent::ToolOutput { chunk, .. } => {
            format!("agent_event tool_output bytes={}", chunk.len())
        }
        AgentEvent::ToolCompleted { status, .. } => {
            format!("agent_event tool_completed status={status:?}")
        }
        AgentEvent::GuardrailTriggered { .. } => "agent_event guardrail_triggered".into(),
        AgentEvent::ExecutionCompleted { .. } => "agent_event execution_completed".into(),
        AgentEvent::ExecutionFailed { .. } => "agent_event execution_failed".into(),
        AgentEvent::ExecutionCancelled { .. } => "agent_event execution_cancelled".into(),
        AgentEvent::ExecutionCompactionRequired { .. } => {
            "agent_event execution_compaction_required".into()
        }
    }
}

fn summarize_host_event(event: &DemoHostEvent) -> String {
    match event {
        DemoHostEvent::AgentExecutionStarted => "host_event agent_execution_started".into(),
        DemoHostEvent::AgentExecutionFinished { .. } => {
            "host_event agent_execution_finished".into()
        }
        DemoHostEvent::CancellationRequested => "host_event cancellation_requested".into(),
        DemoHostEvent::JournalBeginFinished { succeeded } => {
            format!("host_event journal_begin_finished succeeded={succeeded}")
        }
        DemoHostEvent::JournalCommitFinished { succeeded } => {
            format!("host_event journal_commit_finished succeeded={succeeded}")
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_model::{GenerationConfig, ModelRequest, ProviderOptions, SystemPromptSnapshot};
    use agent_types::{ConversationSnapshot, ToolChoice};

    use super::*;
    use crate::trace::{ProviderMetadata, TRACE_FORMAT_VERSION, TraceLayer, TraceStarted};

    #[test]
    fn timeline_hides_sensitive_payload_by_default_and_full_reveals_it() {
        let request = ModelRequest {
            system: SystemPromptSnapshot::new(vec!["secret system prompt".into()]),
            conversation: ConversationSnapshot::new(vec![]),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        };
        let trace = LoadedTrace {
            started: TraceStarted {
                format_version: TRACE_FORMAT_VERSION,
                trace_id: "trace-1".into(),
                provider: ProviderMetadata {
                    adapter: "openai-compatible".into(),
                    adapter_version: 1,
                    protocol_adapter: "generic".into(),
                    provider_id: "fixture".into(),
                    protocol: "openai.chat_completions".into(),
                    endpoint: "https://example.invalid/v1".into(),
                    model: "fixture".into(),
                    context_window_tokens: 4096,
                },
                started_at_ms: 1,
            },
            records: vec![TraceRecord {
                sequence: 1,
                observed_at_ms: 2,
                layer: TraceLayer::Model,
                correlation_id: Some("call-1".into()),
                attempt: None,
                payload: NativeTracePayload::ModelRequest(request),
            }],
            completed: None,
            completeness: TraceCompleteness::Incomplete,
        };

        let safe = render_timeline(&trace, false);
        assert!(safe.contains("model_request system_parts=1 messages=0 tools=0"));
        assert!(!safe.contains("secret system prompt"));

        let full = render_timeline(&trace, true);
        assert!(full.contains("secret system prompt"));
    }
}
