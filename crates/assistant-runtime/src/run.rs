//! Runtime Run 记录、Recorder 与唯一 supervisor 结算链路。

use std::{panic::AssertUnwindSafe, sync::Arc};

use agent_core::{
    AgentEvent, AgentEventStream, CompletionFuture, ExchangeReceipt, ExecutionError,
    ExecutionOutcome, ExecutionRecorder, RecordError, RecordFuture, ToolCompletionStatus,
};
use agent_tools::ToolOutputChannel as AgentToolOutputChannel;
use agent_types::{AssistantMessage, AssistantPart, ConversationMessage, ToolMessage};
use assistant_protocol::{
    PartId as ProtocolPartId, RunId, RunSnapshot, RunStatus, RuntimeErrorCode, RuntimeErrorInfo,
    RuntimeEvent, SessionId, ToolActivitySnapshot, ToolActivityStatus,
    ToolCallId as ProtocolToolCallId, ToolOutputChannel,
};
use futures_util::{FutureExt, StreamExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    RuntimeError, RuntimeResult,
    session::{SessionController, SessionState},
};

/// Session 内保存的 Runtime Run 权威记录。
pub(crate) struct RunRecord {
    run_id: RunId,
    session_id: SessionId,
    status: RunStatus,
    cancel_requested: bool,
    reasoning: String,
    text: String,
    tools: Vec<ToolActivitySnapshot>,
    error: Option<RuntimeErrorInfo>,
}

impl RunRecord {
    pub(crate) fn accepted(run_id: RunId, session_id: SessionId) -> Self {
        Self {
            run_id,
            session_id,
            status: RunStatus::Accepted,
            cancel_requested: false,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: None,
        }
    }

    pub(crate) fn snapshot(&self) -> RunSnapshot {
        RunSnapshot {
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            status: self.status,
            cancel_requested: self.cancel_requested,
            reasoning: self.reasoning.clone(),
            text: self.text.clone(),
            tools: self.tools.clone(),
            error: self.error.clone(),
        }
    }

    pub(crate) fn mark_running(&mut self) -> bool {
        if self.status == RunStatus::Accepted {
            self.status = RunStatus::Running;
            true
        } else {
            false
        }
    }

    pub(crate) fn mark_cancelling(&mut self) -> bool {
        if self.status.is_terminal() || self.status == RunStatus::Cancelling {
            return false;
        }
        self.status = RunStatus::Cancelling;
        self.cancel_requested = true;
        true
    }

    fn settle(&mut self, settlement: RunSettlement) {
        self.status = settlement.status;
        if let Some(reasoning) = settlement.reasoning {
            self.reasoning = reasoning;
        }
        if let Some(text) = settlement.text {
            self.text = text;
        }
        self.error = settlement.error;
    }
}

/// 当前活动 Run 的执行控制句柄；Session 终态后立即清除。
pub(crate) struct ActiveRun {
    pub(crate) run_id: RunId,
    pub(crate) cancellation: CancellationToken,
}

/// 把 Core 的两阶段落账调用绑定到唯一 Session/Run。
pub(crate) struct RuntimeRecorder {
    session: Arc<SessionController>,
    run_id: RunId,
}

impl RuntimeRecorder {
    pub(crate) fn new(session: Arc<SessionController>, run_id: RunId) -> Self {
        Self { session, run_id }
    }
}

impl ExecutionRecorder for RuntimeRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            let mut state = self.lock_state()?;
            if !is_active_run(&state, &self.run_id) {
                state.is_faulted = true;
                return Err(record_error("active run does not match recorder"));
            }
            match state.journal.begin_tool_exchange(&self.run_id, assistant) {
                Ok(receipt) => Ok(receipt),
                Err(_) => {
                    state.is_faulted = true;
                    Err(record_error("journal rejected tool exchange begin"))
                }
            }
        })
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.lock_state()?;
            if !is_active_run(&state, &self.run_id) {
                state.is_faulted = true;
                return Err(record_error("active run does not match recorder"));
            }
            match state
                .journal
                .complete_tool_exchange(&self.run_id, receipt, results)
            {
                Ok(()) => Ok(()),
                Err(_) => {
                    state.is_faulted = true;
                    Err(record_error("journal rejected tool exchange completion"))
                }
            }
        })
    }
}

impl RuntimeRecorder {
    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, SessionState>, RecordError> {
        self.session
            .lock_state()
            .map_err(|_| record_error("session state is unavailable"))
    }
}

/// 同时排空可用观察事件并等待可靠 completion，随后只结算一次 Runtime Run。
pub(crate) async fn supervise_run(
    session: Arc<SessionController>,
    run_id: RunId,
    mut events: AgentEventStream,
    completion: CompletionFuture,
    event_sender: broadcast::Sender<RuntimeEvent>,
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

    // 锁中毒时没有安全的内存恢复方式；错误将在后续查询中显式返回。
    if let Ok(snapshot) = settle_run(&session, &run_id, outcome) {
        let _ = event_sender.send(RuntimeEvent::RunFinished {
            session_id: snapshot.session_id,
            run_id: snapshot.run_id,
            status: snapshot.status,
            error: snapshot.error,
        });
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

pub(crate) fn settle_run(
    session: &SessionController,
    run_id: &RunId,
    outcome: Option<ExecutionOutcome>,
) -> RuntimeResult<RunSnapshot> {
    let mut state = session.lock_state()?;
    if !is_active_run(&state, run_id) {
        state.is_faulted = true;
        return Err(RuntimeError::InternalStateUnavailable {
            component: "run settlement ownership",
        });
    }
    if !state.runs.contains_key(run_id) {
        state.is_faulted = true;
        state.active_run = None;
        return Err(RuntimeError::InternalStateUnavailable {
            component: "run settlement record",
        });
    }

    let mut settlement = match outcome {
        Some(ExecutionOutcome::Completed(message)) => {
            match state
                .journal
                .append_completed(ConversationMessage::Assistant(message.clone()))
            {
                Ok(()) => completed_settlement(&message),
                Err(_) => {
                    state.is_faulted = true;
                    internal_failure("final assistant message could not be committed")
                }
            }
        }
        Some(ExecutionOutcome::Failed(error)) => failed_settlement(&error),
        Some(ExecutionOutcome::Cancelled) => RunSettlement::terminal(RunStatus::Cancelled),
        Some(ExecutionOutcome::CompactionRequired { .. }) => {
            RunSettlement::terminal(RunStatus::CompactionRequired)
        }
        None => internal_failure("agent completion task terminated unexpectedly"),
    };

    if state.journal.has_pending() {
        state.is_faulted = true;
        settlement = internal_failure("run ended with an incomplete tool exchange");
    }

    state
        .runs
        .get_mut(run_id)
        .expect("run existence checked above")
        .settle(settlement);
    let snapshot = state
        .runs
        .get(run_id)
        .expect("run existence checked above")
        .snapshot();
    state.active_run = None;
    Ok(snapshot)
}

fn is_active_run(state: &SessionState, run_id: &RunId) -> bool {
    state
        .active_run
        .as_ref()
        .is_some_and(|active| &active.run_id == run_id)
}

fn completed_settlement(message: &AssistantMessage) -> RunSettlement {
    let mut reasoning = String::new();
    let mut text = String::new();
    for part in &message.parts {
        match part {
            AssistantPart::Reasoning(part) => reasoning.push_str(&part.text),
            AssistantPart::Text(part) => text.push_str(&part.text),
            AssistantPart::ToolCall(_) | AssistantPart::ProviderState(_) => {}
        }
    }
    RunSettlement {
        status: RunStatus::Completed,
        reasoning: Some(reasoning),
        text: Some(text),
        error: None,
    }
}

fn failed_settlement(error: &ExecutionError) -> RunSettlement {
    let message = match error {
        ExecutionError::Model(_) => "model execution failed",
        ExecutionError::ContextWindow(_) => "conversation context is invalid",
        ExecutionError::Record(_) => "conversation could not be recorded",
        ExecutionError::BudgetExceeded { .. } => "execution budget was exceeded",
        ExecutionError::GuardrailTriggered { .. } => "execution guardrail was triggered",
    };
    RunSettlement {
        status: RunStatus::Failed,
        reasoning: None,
        text: None,
        error: Some(RuntimeErrorInfo::new(RuntimeErrorCode::Internal, message)),
    }
}

fn internal_failure(message: &'static str) -> RunSettlement {
    RunSettlement {
        status: RunStatus::Failed,
        reasoning: None,
        text: None,
        error: Some(RuntimeErrorInfo::new(RuntimeErrorCode::Internal, message)),
    }
}

fn record_error(message: &'static str) -> RecordError {
    RecordError {
        message: message.to_owned(),
    }
}

struct RunSettlement {
    status: RunStatus,
    reasoning: Option<String>,
    text: Option<String>,
    error: Option<RuntimeErrorInfo>,
}

impl RunSettlement {
    fn terminal(status: RunStatus) -> Self {
        Self {
            status,
            reasoning: None,
            text: None,
            error: None,
        }
    }
}
