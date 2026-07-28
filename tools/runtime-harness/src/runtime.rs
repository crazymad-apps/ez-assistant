//! Temporary Session and Run orchestration around one Agent execution.

use std::{
    fmt,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use agent_core::{
    AgentEvent, AgentExecution, ExecutionContext, ExecutionControl, ExecutionOutcome, ExecutionSpec,
};
use agent_types::{ConversationMessage, MessageId, PartId, TextPart, UserMessage, UserPart};
use tokio_util::sync::CancellationToken;

use crate::{
    HarnessError,
    journal::{HarnessJournal, HarnessRecorder, JournalSnapshot, PendingSummary},
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HarnessSessionId(String);

impl HarnessSessionId {
    pub(crate) fn from_value(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for HarnessSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HarnessRunId(String);

impl HarnessRunId {
    pub(crate) fn from_sequence(sequence: u64) -> Self {
        Self(format!("run_{sequence}"))
    }
}

impl fmt::Display for HarnessRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for RunStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => formatter.write_str("pending"),
            Self::Running => formatter.write_str("running"),
            Self::Completed => formatter.write_str("completed"),
            Self::Failed => formatter.write_str("failed"),
            Self::Cancelled => formatter.write_str("cancelled"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalKind {
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for TerminalKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed => formatter.write_str("completed"),
            Self::Failed => formatter.write_str("failed"),
            Self::Cancelled => formatter.write_str("cancelled"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

impl fmt::Display for MessageRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System => formatter.write_str("system"),
            Self::User => formatter.write_str("user"),
            Self::Assistant => formatter.write_str("assistant"),
            Self::Tool => formatter.write_str("tool"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunSnapshot {
    pub(crate) id: HarnessRunId,
    pub(crate) status: RunStatus,
    pub(crate) created_at_ms: Option<u128>,
    pub(crate) started_at_ms: Option<u128>,
    pub(crate) finished_at_ms: Option<u128>,
    pub(crate) elapsed: Option<Duration>,
    pub(crate) event_count: u64,
    pub(crate) terminal: Option<TerminalKind>,
    pub(crate) dropped_events: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSnapshot {
    pub(crate) session_id: HarnessSessionId,
    pub(crate) run: Option<RunSnapshot>,
    pub(crate) completed_messages: usize,
    pub(crate) roles: Vec<MessageRole>,
    pub(crate) pending: Vec<PendingSummary>,
}

pub(crate) struct PreparedRun {
    pub(crate) run_id: HarnessRunId,
    pub(crate) input: agent_core::ExecutionInput,
    pub(crate) recorder: Arc<HarnessRecorder>,
}

struct EventSummary {
    count: u64,
    terminal: Option<TerminalKind>,
    dropped_events: u64,
}

struct HarnessRun {
    id: HarnessRunId,
    status: RunStatus,
    created_at: SystemTime,
    started_at: Option<SystemTime>,
    finished_at: Option<SystemTime>,
    started_instant: Option<Instant>,
    elapsed: Option<Duration>,
    control: Option<ExecutionControl>,
    outcome: Option<ExecutionOutcome>,
    events: EventSummary,
}

impl HarnessRun {
    fn new(id: HarnessRunId) -> Self {
        Self {
            id,
            status: RunStatus::Pending,
            created_at: SystemTime::now(),
            started_at: None,
            finished_at: None,
            started_instant: None,
            elapsed: None,
            control: None,
            outcome: None,
            events: EventSummary {
                count: 0,
                terminal: None,
                dropped_events: 0,
            },
        }
    }

    fn transition(&mut self, next: RunStatus) -> Result<(), HarnessError> {
        let is_valid = matches!(
            (self.status, next),
            (RunStatus::Pending, RunStatus::Running)
                | (
                    RunStatus::Running,
                    RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
                )
        );
        if !is_valid {
            return Err(HarnessError::Execution(format!(
                "invalid run transition: {} -> {next}",
                self.status
            )));
        }
        self.status = next;
        Ok(())
    }

    fn mark_running(&mut self) -> Result<(), HarnessError> {
        self.transition(RunStatus::Running)?;
        self.started_at = Some(SystemTime::now());
        self.started_instant = Some(Instant::now());
        Ok(())
    }

    fn finish(&mut self, outcome: ExecutionOutcome) -> Result<(), HarnessError> {
        let (status, terminal) = match &outcome {
            ExecutionOutcome::Completed(_) => (RunStatus::Completed, TerminalKind::Completed),
            ExecutionOutcome::Failed(_) => (RunStatus::Failed, TerminalKind::Failed),
            ExecutionOutcome::Cancelled => (RunStatus::Cancelled, TerminalKind::Cancelled),
        };
        self.transition(status)?;
        self.finished_at = Some(SystemTime::now());
        self.elapsed = self.started_instant.map(|started| started.elapsed());
        if self.events.terminal.is_none() {
            self.events.terminal = Some(terminal);
        }
        self.outcome = Some(outcome);
        Ok(())
    }

    fn observe(&mut self, event: &AgentEvent) {
        self.events.count += 1;
        match event {
            AgentEvent::ExecutionCompleted { dropped_events, .. } => {
                self.events.terminal = Some(TerminalKind::Completed);
                self.events.dropped_events = *dropped_events;
            }
            AgentEvent::ExecutionFailed { dropped_events, .. } => {
                self.events.terminal = Some(TerminalKind::Failed);
                self.events.dropped_events = *dropped_events;
            }
            AgentEvent::ExecutionCancelled { dropped_events } => {
                self.events.terminal = Some(TerminalKind::Cancelled);
                self.events.dropped_events = *dropped_events;
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> RunSnapshot {
        let elapsed = match (self.elapsed, self.started_instant) {
            (Some(elapsed), _) => Some(elapsed),
            (None, Some(started)) => Some(started.elapsed()),
            (None, None) => None,
        };
        RunSnapshot {
            id: self.id.clone(),
            status: self.status,
            created_at_ms: unix_ms(self.created_at),
            started_at_ms: self.started_at.and_then(unix_ms),
            finished_at_ms: self.finished_at.and_then(unix_ms),
            elapsed,
            event_count: self.events.count,
            terminal: self.events.terminal,
            dropped_events: self.events.dropped_events,
        }
    }
}

pub(crate) struct HarnessRuntime {
    session_id: HarnessSessionId,
    journal: Arc<HarnessJournal>,
    next_run: u64,
    active_run: Option<HarnessRun>,
    recent_run: Option<HarnessRun>,
}

impl HarnessRuntime {
    pub(crate) fn new() -> Result<Self, HarnessError> {
        let start_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| HarnessError::Config("system clock is before Unix epoch".to_owned()))?
            .as_millis();
        Ok(Self::with_session_id(HarnessSessionId::from_value(
            format!("session_{}_{}", std::process::id(), start_ms),
        )))
    }

    fn with_session_id(session_id: HarnessSessionId) -> Self {
        Self {
            session_id,
            journal: HarnessJournal::new(),
            next_run: 1,
            active_run: None,
            recent_run: None,
        }
    }

    pub(crate) fn for_scenario(name: &str) -> Self {
        Self::with_session_id(HarnessSessionId::from_value(format!(
            "session_verify_{name}"
        )))
    }

    pub(crate) fn prepare_run(&mut self, text: &str) -> Result<PreparedRun, HarnessError> {
        if self.active_run.is_some() {
            return Err(HarnessError::Execution(
                "another run is already active".to_owned(),
            ));
        }
        if self.journal.has_pending()? {
            return Err(crate::journal::JournalError::PendingBlocksRun.into());
        }
        if text.trim().is_empty() {
            return Err(HarnessError::Config(
                "user input must not be empty".to_owned(),
            ));
        }

        let run_id = HarnessRunId::from_sequence(self.next_run);
        let snapshot = self.journal.snapshot()?.conversation;
        let user = user_message(&run_id, text)?;
        self.journal.append_user(user.clone())?;
        self.next_run += 1;
        self.active_run = Some(HarnessRun::new(run_id.clone()));

        Ok(PreparedRun {
            run_id: run_id.clone(),
            input: agent_core::ExecutionInput {
                conversation: snapshot,
                user_input: user,
            },
            recorder: HarnessRecorder::new(Arc::clone(&self.journal), run_id),
        })
    }

    #[cfg(test)]
    pub(crate) fn start_execution(
        &mut self,
        spec: ExecutionSpec,
        text: &str,
    ) -> Result<(HarnessRunId, AgentExecution), HarnessError> {
        let prepared = self.prepare_run(text)?;
        self.start_prepared(spec, prepared)
    }

    pub(crate) fn start_prepared(
        &mut self,
        spec: ExecutionSpec,
        prepared: PreparedRun,
    ) -> Result<(HarnessRunId, AgentExecution), HarnessError> {
        let run_id = prepared.run_id.clone();
        let execution = AgentExecution::start(
            spec,
            prepared.input,
            ExecutionContext {
                cancellation: CancellationToken::new(),
                recorder: prepared.recorder,
                authorizer: Arc::new(agent_core::AllowAllAuthorizer),
            },
        );
        self.mark_running(&run_id)?;
        self.attach_control(&run_id, execution.control.clone())?;
        Ok((run_id, execution))
    }

    pub(crate) fn mark_running(&mut self, run_id: &HarnessRunId) -> Result<(), HarnessError> {
        self.active_run_mut(run_id)?.mark_running()
    }

    pub(crate) fn attach_control(
        &mut self,
        run_id: &HarnessRunId,
        control: ExecutionControl,
    ) -> Result<(), HarnessError> {
        let run = self.active_run_mut(run_id)?;
        if run.status != RunStatus::Running {
            return Err(HarnessError::Execution(
                "execution control can only attach to a running run".to_owned(),
            ));
        }
        if run.control.is_some() {
            return Err(HarnessError::Execution(
                "execution control is already attached".to_owned(),
            ));
        }
        run.control = Some(control);
        Ok(())
    }

    pub(crate) fn cancel_active(&self) -> Result<(), HarnessError> {
        let run = self.active_run.as_ref().ok_or_else(|| {
            HarnessError::Execution("there is no active run to cancel".to_owned())
        })?;
        let control = run.control.as_ref().ok_or_else(|| {
            HarnessError::Execution("active run has no execution control yet".to_owned())
        })?;
        control.cancel();
        Ok(())
    }

    pub(crate) fn observe_event(
        &mut self,
        run_id: &HarnessRunId,
        event: &AgentEvent,
    ) -> Result<(), HarnessError> {
        let run = self.active_run_mut(run_id)?;
        if run.status != RunStatus::Running {
            return Err(HarnessError::Execution(
                "events can only be observed for a running run".to_owned(),
            ));
        }
        run.observe(event);
        Ok(())
    }

    pub(crate) fn finish_run(
        &mut self,
        run_id: &HarnessRunId,
        outcome: ExecutionOutcome,
    ) -> Result<(), HarnessError> {
        {
            let run = self.active_run_ref(run_id)?;
            if run.status != RunStatus::Running {
                return Err(HarnessError::Execution(
                    "only a running run can finish".to_owned(),
                ));
            }
        }
        if let ExecutionOutcome::Completed(message) = &outcome {
            self.journal.append_assistant(message.clone())?;
        }
        self.active_run_mut(run_id)?.finish(outcome)?;
        self.recent_run = self.active_run.take();
        Ok(())
    }

    pub(crate) fn reset(&mut self) -> Result<(), HarnessError> {
        if self.active_run.is_some() {
            return Err(HarnessError::Execution(
                "cannot reset while a run is active".to_owned(),
            ));
        }
        self.journal.reset()?;
        self.recent_run = None;
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Result<RuntimeSnapshot, HarnessError> {
        let journal = self.journal.snapshot()?;
        Ok(RuntimeSnapshot {
            session_id: self.session_id.clone(),
            run: self
                .active_run
                .as_ref()
                .or(self.recent_run.as_ref())
                .map(HarnessRun::snapshot),
            completed_messages: journal.conversation.messages.len(),
            roles: message_roles(&journal),
            pending: journal.pending,
        })
    }

    pub(crate) fn correlation_id(&self, run_id: &HarnessRunId) -> String {
        format!("{}/{run_id}", self.session_id)
    }

    fn active_run_ref(&self, run_id: &HarnessRunId) -> Result<&HarnessRun, HarnessError> {
        self.active_run
            .as_ref()
            .filter(|run| run.id == *run_id)
            .ok_or_else(|| HarnessError::Execution(format!("run `{run_id}` is not active")))
    }

    fn active_run_mut(&mut self, run_id: &HarnessRunId) -> Result<&mut HarnessRun, HarnessError> {
        self.active_run
            .as_mut()
            .filter(|run| run.id == *run_id)
            .ok_or_else(|| HarnessError::Execution(format!("run `{run_id}` is not active")))
    }
}

fn user_message(run_id: &HarnessRunId, text: &str) -> Result<UserMessage, HarnessError> {
    let message_id = MessageId::new(format!("{run_id}_user"))
        .map_err(crate::journal::JournalError::InvalidIdentifier)?;
    let part_id = PartId::new(format!("{run_id}_user_text"))
        .map_err(crate::journal::JournalError::InvalidIdentifier)?;
    Ok(UserMessage {
        id: message_id,
        parts: vec![UserPart::Text(TextPart {
            id: part_id,
            text: text.to_owned(),
        })],
    })
}

fn message_roles(snapshot: &JournalSnapshot) -> Vec<MessageRole> {
    snapshot
        .conversation
        .messages
        .iter()
        .map(|message| match message {
            ConversationMessage::System(_) => MessageRole::System,
            ConversationMessage::User(_) => MessageRole::User,
            ConversationMessage::Assistant(_) => MessageRole::Assistant,
            ConversationMessage::Tool(_) => MessageRole::Tool,
        })
        .collect()
}

fn unix_ms(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis())
}

#[cfg(test)]
mod tests {
    use agent_core::{BudgetKind, ExecutionBudget, ExecutionError, ExecutionRecorder};
    use agent_model::{ModelCapabilities, ModelService};
    use agent_testkit::{ModelScript, ScriptedModelService, message_events};
    use agent_tools::{ToolRegistry, ToolSetSnapshot};
    use agent_types::{
        AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, ProviderId,
        ToolCall, ToolCallId, ToolName,
    };
    use futures_util::StreamExt;
    use serde_json::json;

    use super::*;
    use crate::demo_tool::LookupWeatherTool;

    fn runtime() -> HarnessRuntime {
        HarnessRuntime::with_session_id(HarnessSessionId::from_value("session_test"))
    }

    fn assistant(id: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(id).expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("scripted").expect("valid provider id"),
                "scripted-model",
            ),
            parts: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    fn failure() -> ExecutionOutcome {
        ExecutionOutcome::Failed(ExecutionError::BudgetExceeded {
            kind: BudgetKind::Steps,
            limit: 1,
        })
    }

    fn tool_message(id: &str) -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new(id).expect("valid message id"),
            model: ModelIdentity::new(
                ProviderId::new("scripted").expect("valid provider id"),
                "scripted-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("weather_call").expect("valid tool call id"),
                name: ToolName::new("lookup_weather").expect("valid tool name"),
                arguments: json!({"city": "Shanghai"}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }
    }

    fn weather_tools() -> ToolSetSnapshot {
        let mut registry = ToolRegistry::new();
        registry
            .register(LookupWeatherTool)
            .expect("register weather tool");
        registry.snapshot()
    }

    async fn execute_scripted_run(
        runtime: &mut HarnessRuntime,
        model: Arc<dyn ModelService>,
        tools: ToolSetSnapshot,
        text: &str,
    ) {
        let spec = ExecutionSpec {
            instructions: vec!["offline interactive test".to_owned()],
            model,
            tools,
            budget: ExecutionBudget {
                max_steps: Some(8),
                max_tool_calls: Some(8),
            },
        };
        let (run_id, execution) = runtime
            .start_execution(spec, text)
            .expect("start execution");
        let AgentExecution {
            events,
            completion,
            control: _,
        } = execution;
        let events = events.collect::<Vec<_>>().await;
        let outcome = completion.await;
        for event in &events {
            runtime
                .observe_event(&run_id, event)
                .expect("observe event");
        }
        runtime
            .finish_run(&run_id, outcome)
            .expect("finish execution");
    }

    #[test]
    fn session_and_run_ids_are_stable_and_monotonic() {
        let mut runtime = runtime();
        let first = runtime.prepare_run("one").expect("prepare first");
        assert_eq!(first.run_id, HarnessRunId::from_sequence(1));
        assert_eq!(runtime.correlation_id(&first.run_id), "session_test/run_1");
        runtime.mark_running(&first.run_id).expect("start first");
        runtime
            .finish_run(&first.run_id, ExecutionOutcome::Cancelled)
            .expect("finish first");
        runtime.reset().expect("reset");

        let second = runtime.prepare_run("two").expect("prepare second");
        assert_eq!(second.run_id, HarnessRunId::from_sequence(2));
    }

    #[test]
    fn rejects_parallel_and_invalid_state_transitions() {
        let mut runtime = runtime();
        let prepared = runtime.prepare_run("one").expect("prepare");
        assert!(runtime.prepare_run("parallel").is_err());
        assert!(
            runtime
                .finish_run(&prepared.run_id, ExecutionOutcome::Cancelled)
                .is_err()
        );
        runtime.mark_running(&prepared.run_id).expect("start");
        assert!(runtime.mark_running(&prepared.run_id).is_err());
        assert!(runtime.reset().is_err());
    }

    #[tokio::test]
    async fn pending_exchange_blocks_new_run_until_reset() {
        let mut runtime = runtime();
        let prepared = runtime.prepare_run("use a tool").expect("prepare");
        runtime.mark_running(&prepared.run_id).expect("start");
        prepared
            .recorder
            .begin_tool_exchange(assistant("assistant_tool"))
            .await
            .expect("begin pending");
        runtime
            .finish_run(&prepared.run_id, failure())
            .expect("finish failed run");
        assert!(runtime.prepare_run("blocked").is_err());

        runtime.reset().expect("reset clears pending");
        let next = runtime.prepare_run("allowed").expect("prepare after reset");
        assert_eq!(next.run_id, HarnessRunId::from_sequence(2));
        assert!(next.input.conversation.messages.is_empty());
    }

    #[test]
    fn only_completed_outcomes_append_a_final_assistant_message() {
        let mut runtime = runtime();

        let completed = runtime.prepare_run("complete").expect("prepare completed");
        assert!(completed.input.conversation.messages.is_empty());
        runtime.mark_running(&completed.run_id).expect("start");
        runtime
            .finish_run(
                &completed.run_id,
                ExecutionOutcome::Completed(assistant("assistant_final")),
            )
            .expect("finish completed");
        let snapshot = runtime.snapshot().expect("snapshot");
        assert_eq!(
            snapshot.roles,
            vec![MessageRole::User, MessageRole::Assistant]
        );

        let failed = runtime.prepare_run("fail").expect("prepare failed");
        assert_eq!(failed.input.conversation.messages.len(), 2);
        runtime.mark_running(&failed.run_id).expect("start");
        runtime
            .finish_run(&failed.run_id, failure())
            .expect("finish failed");

        let cancelled = runtime.prepare_run("cancel").expect("prepare cancelled");
        runtime.mark_running(&cancelled.run_id).expect("start");
        runtime
            .finish_run(&cancelled.run_id, ExecutionOutcome::Cancelled)
            .expect("finish cancelled");
        let snapshot = runtime.snapshot().expect("snapshot");
        assert_eq!(
            snapshot.roles,
            vec![
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::User,
                MessageRole::User,
            ]
        );
    }

    #[test]
    fn event_summary_records_terminal_and_dropped_count() {
        let mut runtime = runtime();
        let prepared = runtime.prepare_run("observe").expect("prepare");
        runtime.mark_running(&prepared.run_id).expect("start");
        runtime
            .observe_event(&prepared.run_id, &AgentEvent::ExecutionStarted)
            .expect("observe start");
        runtime
            .observe_event(
                &prepared.run_id,
                &AgentEvent::ExecutionCancelled { dropped_events: 3 },
            )
            .expect("observe terminal");
        runtime
            .finish_run(&prepared.run_id, ExecutionOutcome::Cancelled)
            .expect("finish");
        let run = runtime
            .snapshot()
            .expect("snapshot")
            .run
            .expect("recent run");
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(run.event_count, 2);
        assert_eq!(run.terminal, Some(TerminalKind::Cancelled));
        assert_eq!(run.dropped_events, 3);
        assert!(run.elapsed.is_some());
        assert!(run.created_at_ms.is_some());
        assert!(run.started_at_ms.is_some());
        assert!(run.finished_at_ms.is_some());
        assert!(run.created_at_ms <= run.started_at_ms);
        assert!(run.started_at_ms <= run.finished_at_ms);
    }

    #[tokio::test]
    async fn two_interactive_inputs_share_completed_tool_journal() {
        let tool_turn = tool_message("assistant_tool");
        let first_final = assistant("assistant_first_final");
        let second_final = assistant("assistant_second_final");
        let model = Arc::new(ScriptedModelService::new(
            ModelCapabilities {
                reasoning: true,
                tool_calls: true,
                streaming: true,
            },
            [
                ModelScript::Events(message_events(&tool_turn)),
                ModelScript::Events(message_events(&first_final)),
                ModelScript::Events(message_events(&second_final)),
            ],
        ));
        let mut runtime = runtime();
        let service: Arc<dyn ModelService> = model.clone();

        execute_scripted_run(
            &mut runtime,
            Arc::clone(&service),
            weather_tools(),
            "What is the demo weather?",
        )
        .await;
        execute_scripted_run(
            &mut runtime,
            service,
            weather_tools(),
            "What did I ask previously?",
        )
        .await;

        assert_eq!(
            runtime.snapshot().expect("snapshot").roles,
            vec![
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::Tool,
                MessageRole::Assistant,
                MessageRole::User,
                MessageRole::Assistant,
            ]
        );
        let requests = model.take_requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[2].conversation.messages.len(), 5);
    }
}
