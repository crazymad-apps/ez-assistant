//! Core 事件投影与 Run completion 监督；可靠终态提交由 settlement 模块负责。

use std::{panic::AssertUnwindSafe, sync::Arc};

use agent_core::{AgentEvent, AgentEventStream, CompletionFuture, ToolCompletionStatus};
use agent_tools::ToolOutputChannel as AgentToolOutputChannel;
use assistant_protocol::{
    PartId as ProtocolPartId, RunId, RuntimeEvent, ToolActivitySnapshot, ToolActivityStatus,
    ToolCallId as ProtocolToolCallId, ToolOutputChannel,
};
use futures_util::{FutureExt, StreamExt};
use tokio::sync::broadcast;

use crate::{RuntimeError, RuntimeResult, RuntimeStore, session::SessionController};

use super::{RunRecord, is_active_run, settlement::settle_run};

/// 同时排空可用观察事件并等待可靠 completion，随后只结算一次 Runtime Run。
pub(crate) async fn supervise_run(
    session: Arc<SessionController>,
    run_id: RunId,
    mut events: AgentEventStream,
    completion: CompletionFuture,
    event_sender: broadcast::Sender<RuntimeEvent>,
    store: Arc<dyn RuntimeStore>,
) {
    let completion = AssertUnwindSafe(completion).catch_unwind();
    tokio::pin!(completion);
    let mut events_open = true;

    let outcome = loop {
        tokio::select! {
            biased;
            event = events.next(), if events_open => {
                match event {
                    Some(event) => {
                        if let Ok(Some(event)) = project_agent_event(&session, &run_id, event) {
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

    match settle_run(&session, &run_id, outcome, store.as_ref()).await {
        Ok(snapshot) => {
            let _ = event_sender.send(RuntimeEvent::RunFinished {
                session_id: snapshot.session_id,
                run_id: snapshot.run_id,
                status: snapshot.status,
                error: snapshot.error,
            });
        }
        Err(_) => {
            // 结算失败不能被当成正常 supervisor 退出；即使锁已中毒而无法写入，后续
            // 查询也会显式返回错误，不能让新输入继续修改该 Session。
            let _ = session.mark_faulted();
        }
    }
}

fn project_agent_event(
    session: &SessionController,
    run_id: &RunId,
    event: AgentEvent,
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
        AgentEvent::StepStarted { .. }
        | AgentEvent::UsageUpdated { .. }
        | AgentEvent::GuardrailTriggered { .. }
        | AgentEvent::ExecutionCompleted { .. }
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
