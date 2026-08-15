//! Core 事件投影与 Run completion 监督；可靠终态提交由 settlement 模块负责。

use std::{panic::AssertUnwindSafe, sync::Arc};

use crate::{
    RuntimeError, RuntimeResult, observation::ObservationCoordinator, session::SessionController,
};
use agent_core::{AgentEvent, AgentEventStream, CompletionFuture, ToolCompletionStatus};
use agent_tools::ToolOutputChannel as AgentToolOutputChannel;
use assistant_protocol::{
    PartId as ProtocolPartId, RunId, RuntimeEvent, TokenUsageSnapshot, ToolActivitySnapshot,
    ToolActivityStatus, ToolCallId as ProtocolToolCallId, ToolOutputChannel,
};
use futures_util::{FutureExt, StreamExt};

use super::{RunModelDiagnostics, RunRecord, is_active_run};

/// 同时排空一次 Core execution 的观察事件并等待可靠 completion。
///
/// `CompactionRequired` 不是业务 Run 终态；调用方拿到 outcome 后可以压缩并启动下一次
/// Core execution，只有最终 outcome 才交给 settlement。
pub(crate) async fn observe_run_execution(
    session: Arc<SessionController>,
    run_id: RunId,
    mut events: AgentEventStream,
    completion: CompletionFuture,
    event_sender: ObservationCoordinator,
    model_diagnostics: Arc<RunModelDiagnostics>,
) -> ExecutionObservation {
    let completion = AssertUnwindSafe(completion).catch_unwind();
    tokio::pin!(completion);
    let mut events_open = true;
    let outcome = loop {
        tokio::select! {
            biased;
            event = events.next(), if events_open => {
                match event {
                    Some(event) => {
                        if let Ok(Some(event)) = project_agent_event(
                            &session,
                            &run_id,
                            event,
                            model_diagnostics.as_ref(),
                        ) {
                            let _ = event_sender.send(event);
                        }
                    }
                    None => events_open = false,
                }
            }
            result = &mut completion => break result.ok(),
        }
    };
    drop(events);

    ExecutionObservation { outcome }
}

pub(crate) struct ExecutionObservation {
    pub(crate) outcome: Option<agent_core::ExecutionOutcome>,
}

fn project_agent_event(
    session: &SessionController,
    run_id: &RunId,
    event: AgentEvent,
    model_diagnostics: &RunModelDiagnostics,
) -> RuntimeResult<Option<RuntimeEvent>> {
    if event.is_terminal() {
        return Ok(None);
    }
    let session_id = session.id().clone();
    match event {
        AgentEvent::ExecutionStarted => {
            let transitioned = with_active_record(session, run_id, RunRecord::mark_running)?;
            Ok(transitioned.then(|| RuntimeEvent::RunStarted {
                session_id,
                run_id: run_id.clone(),
            }))
        }
        AgentEvent::TextDelta { id, delta } => {
            model_diagnostics.mark_output_observed();
            let part_id = ProtocolPartId::new(id.as_str()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "runtime text part id",
                }
            })?;
            with_active_record(session, run_id, |record| record.text.push_str(&delta))?;
            Ok(Some(RuntimeEvent::TextDelta {
                session_id,
                run_id: run_id.clone(),
                part_id,
                delta,
            }))
        }
        AgentEvent::ReasoningDelta { id, delta } => {
            model_diagnostics.mark_output_observed();
            let part_id = ProtocolPartId::new(id.as_str()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "runtime reasoning part id",
                }
            })?;
            with_active_record(session, run_id, |record| record.reasoning.push_str(&delta))?;
            Ok(Some(RuntimeEvent::ReasoningDelta {
                session_id,
                run_id: run_id.clone(),
                part_id,
                delta,
            }))
        }
        AgentEvent::UsageUpdated { step, usage } => Ok(Some(RuntimeEvent::UsageUpdated {
            session_id,
            run_id: run_id.clone(),
            step,
            usage: TokenUsageSnapshot {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                cached_input_tokens: usage.cached_input_tokens,
            },
        })),
        AgentEvent::ToolProposed { call } => {
            let call_id = ProtocolToolCallId::new(call.id.as_str()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "runtime tool call id",
                }
            })?;
            let tool_name = call.name.as_str().to_owned();
            with_active_record(session, run_id, |record| {
                if !record.tools.iter().any(|tool| tool.call_id == call_id) {
                    record.tools.push(ToolActivitySnapshot {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        status: ToolActivityStatus::Proposed,
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
            })?;
            Ok(Some(RuntimeEvent::ToolProposed {
                session_id,
                run_id: run_id.clone(),
                call_id,
                tool_name,
            }))
        }
        AgentEvent::ToolStarted { call_id } => {
            let call_id = ProtocolToolCallId::new(call_id.as_str()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "runtime tool call id",
                }
            })?;
            with_active_record(session, run_id, |record| {
                if let Some(tool) = find_tool_mut(record, &call_id) {
                    tool.status = ToolActivityStatus::Running;
                }
            })?;
            Ok(Some(RuntimeEvent::ToolStarted {
                session_id,
                run_id: run_id.clone(),
                call_id,
            }))
        }
        AgentEvent::ToolOutput {
            call_id,
            channel,
            chunk,
        } => {
            let call_id = ProtocolToolCallId::new(call_id.as_str()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "runtime tool call id",
                }
            })?;
            let protocol_channel = match channel {
                AgentToolOutputChannel::Stdout => ToolOutputChannel::Stdout,
                AgentToolOutputChannel::Stderr => ToolOutputChannel::Stderr,
            };
            with_active_record(session, run_id, |record| {
                if let Some(tool) = find_tool_mut(record, &call_id) {
                    match protocol_channel {
                        ToolOutputChannel::Stdout => tool.stdout.push_str(&chunk),
                        ToolOutputChannel::Stderr => tool.stderr.push_str(&chunk),
                    }
                }
            })?;
            Ok(Some(RuntimeEvent::ToolOutput {
                session_id,
                run_id: run_id.clone(),
                call_id,
                channel: protocol_channel,
                chunk,
            }))
        }
        AgentEvent::ToolCompleted { call_id, status } => {
            let call_id = ProtocolToolCallId::new(call_id.as_str()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "runtime tool call id",
                }
            })?;
            let status = match status {
                ToolCompletionStatus::Success => ToolActivityStatus::Completed,
                ToolCompletionStatus::Failed => ToolActivityStatus::Failed,
            };
            with_active_record(session, run_id, |record| {
                if let Some(tool) = find_tool_mut(record, &call_id) {
                    tool.status = status;
                }
            })?;
            Ok(Some(RuntimeEvent::ToolCompleted {
                session_id,
                run_id: run_id.clone(),
                call_id,
                status,
            }))
        }
        AgentEvent::StepStarted { step } => {
            model_diagnostics.mark_step_started();
            with_active_record(session, run_id, RunRecord::start_step)?;
            Ok(Some(RuntimeEvent::StepStarted {
                session_id,
                run_id: run_id.clone(),
                step,
            }))
        }
        AgentEvent::GuardrailTriggered {
            kind,
            mode,
            threshold,
            observed,
            call_id,
        } => {
            let call_id = ProtocolToolCallId::new(call_id.as_str()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "runtime guardrail tool call id",
                }
            })?;
            let kind = match kind {
                agent_core::GuardrailKind::RepeatedInvocation => {
                    assistant_protocol::GuardrailKind::RepeatedInvocation
                }
                agent_core::GuardrailKind::ConsecutiveFailures => {
                    assistant_protocol::GuardrailKind::ConsecutiveFailures
                }
            };
            let mode = match mode {
                agent_core::ActiveGuardrailMode::Observe => {
                    assistant_protocol::GuardrailMode::Observe
                }
                agent_core::ActiveGuardrailMode::Enforce => {
                    assistant_protocol::GuardrailMode::Enforce
                }
            };
            Ok(Some(RuntimeEvent::GuardrailTriggered {
                session_id,
                run_id: run_id.clone(),
                call_id,
                kind,
                mode,
                threshold: threshold.get(),
                observed,
            }))
        }
        AgentEvent::ExecutionCompleted { .. }
        | AgentEvent::ExecutionFailed { .. }
        | AgentEvent::ExecutionCancelled { .. }
        | AgentEvent::ExecutionCompactionRequired { .. } => Ok(None),
    }
}

fn with_active_record<Output>(
    session: &SessionController,
    run_id: &RunId,
    update: impl FnOnce(&mut RunRecord) -> Output,
) -> RuntimeResult<Output> {
    let mut state = session.lock_state()?;
    if !is_active_run(&state, run_id) {
        state.is_faulted = true;
        return Err(RuntimeError::InternalStateUnavailable {
            component: "run event ownership",
        });
    }
    let Some(record) = state.runs.get_mut(run_id) else {
        state.is_faulted = true;
        return Err(RuntimeError::InternalStateUnavailable {
            component: "run event record",
        });
    };
    Ok(update(record))
}

fn find_tool_mut<'a>(
    record: &'a mut RunRecord,
    call_id: &ProtocolToolCallId,
) -> Option<&'a mut ToolActivitySnapshot> {
    record
        .tools
        .iter_mut()
        .find(|tool| &tool.call_id == call_id)
}
