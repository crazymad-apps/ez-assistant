//! Representative offline scenarios for the v0.2 capability baseline.

use std::sync::Arc;

use agent_context::ContextWindowEvaluator;
use agent_core::{
    AgentEvent, AgentExecution, ExecutionBudget, ExecutionContext, ExecutionError,
    ExecutionOutcome, ExecutionSpec, ToolAuthorization, ToolAuthorizer, ToolCompletionStatus,
};
use agent_model::{
    ModelCallContext, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelRequest,
    ModelService, ModelStreamFuture, ModelTransportErrorKind, SystemPromptSnapshot,
};
use agent_testkit::{
    LogEntry, ModelScript, OrderLog, ScriptedAuthorizer, ScriptedModelService, ScriptedTool,
    message_events,
};
use agent_tools::{ToolRegistry, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, PartId, ProviderId,
    TextPart, ToolCall, ToolCallId, ToolName,
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{
    HarnessError,
    output::{format_event, format_outcome},
    runtime::{
        HarnessRunId, HarnessRuntime, MessageRole, RunStatus, RuntimeSnapshot, TerminalKind,
    },
    scenario::{ScenarioFuture, ScenarioReport, ScenarioStatus},
};

const TEST_CONTEXT_WINDOW_TOKENS: u64 = 128_000;

struct ExecutionEvidence {
    report: ScenarioReport,
    outcome: ExecutionOutcome,
    events: Vec<AgentEvent>,
}

pub(super) fn plain_text() -> ScenarioFuture {
    Box::pin(run_plain_text())
}

pub(super) fn single_tool_loop() -> ScenarioFuture {
    Box::pin(run_single_tool_loop())
}

pub(super) fn allow_deny_batch() -> ScenarioFuture {
    Box::pin(run_allow_deny_batch())
}

pub(super) fn controlled_failure() -> ScenarioFuture {
    Box::pin(run_controlled_failure())
}

pub(super) fn cancelled() -> ScenarioFuture {
    Box::pin(run_cancelled())
}

pub(super) fn observation_disconnect() -> ScenarioFuture {
    Box::pin(run_observation_disconnect())
}

async fn run_plain_text() -> Result<ScenarioReport, HarnessError> {
    let final_message = text_message("plain_final", "offline plain text completed")?;
    let model = Arc::new(ScriptedModelService::completing(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        final_message.clone(),
    ));
    let evidence = execute_collected(
        "plain_text",
        model,
        ToolSetSnapshot::default(),
        Arc::new(agent_core::AllowAllAuthorizer),
        "Say hello without tools.",
    )
    .await?;

    ensure(
        evidence.outcome == ExecutionOutcome::Completed(final_message),
        "plain_text did not complete with the scripted message",
    )?;
    ensure(
        count_steps(&evidence.events) == 1,
        "plain_text must use exactly one model step",
    )?;
    ensure(
        evidence.report.journal_roles == [MessageRole::User, MessageRole::Assistant],
        "plain_text journal must contain User -> Assistant",
    )?;
    Ok(evidence.report)
}

async fn run_single_tool_loop() -> Result<ScenarioReport, HarnessError> {
    let tool_turn = calls_message(
        "tool_turn",
        vec![tool_call("date_call", "get_date", json!({}))?],
    )?;
    let final_message = text_message("tool_final", "The scripted date is 2026-07-28.")?;
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&tool_turn)),
            ModelScript::Events(message_events(&final_message)),
        ],
    ));
    let log = OrderLog::new();
    let tools = tool_snapshot(vec![ScriptedTool::succeed(
        "get_date",
        json!({"date": "2026-07-28"}),
        log.clone(),
    )])?;
    let evidence = execute_collected(
        "single_tool_loop",
        model.clone(),
        tools,
        Arc::new(ScriptedAuthorizer::allow_all(log.clone())),
        "Use the date tool, then answer.",
    )
    .await?;

    ensure(
        evidence.outcome == ExecutionOutcome::Completed(final_message),
        "single_tool_loop did not reach its scripted final message",
    )?;
    ensure(
        count_steps(&evidence.events) == 2,
        "single_tool_loop must use two model steps",
    )?;
    ensure(
        evidence.events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::ToolCompleted {
                    status: ToolCompletionStatus::Success,
                    ..
                }
            )
        }),
        "single_tool_loop did not expose a successful tool completion",
    )?;
    ensure(
        evidence.report.journal_roles
            == [
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::Tool,
                MessageRole::Assistant,
            ],
        "single_tool_loop journal projection is incomplete",
    )?;
    ensure(
        log.entries().contains(&LogEntry::ToolExecute {
            name: "get_date".to_owned(),
        }),
        "single_tool_loop did not execute get_date",
    )?;
    let requests = model.take_requests();
    ensure(
        requests.len() == 2 && requests[1].conversation.messages.len() == 3,
        "single_tool_loop did not feed the completed exchange into step two",
    )?;
    Ok(evidence.report)
}

async fn run_allow_deny_batch() -> Result<ScenarioReport, HarnessError> {
    let tool_turn = calls_message(
        "batch_turn",
        vec![
            tool_call("read_call", "read_demo", json!({"path": "demo.txt"}))?,
            tool_call("write_call", "write_demo", json!({"path": "demo.txt"}))?,
        ],
    )?;
    let final_message = text_message(
        "batch_final",
        "The read succeeded and the write was denied.",
    )?;
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&tool_turn)),
            ModelScript::Events(message_events(&final_message)),
        ],
    ));
    let log = OrderLog::new();
    let tools = tool_snapshot(vec![
        ScriptedTool::succeed("read_demo", json!({"content": "offline"}), log.clone()),
        ScriptedTool::succeed("write_demo", json!({"written": true}), log.clone()),
    ])?;
    let authorizer = ScriptedAuthorizer::with_decisions(
        log.clone(),
        [(
            "write_demo".to_owned(),
            ToolAuthorization::Deny {
                reason: "writes are disabled in this scenario".to_owned(),
            },
        )],
    );
    let evidence = execute_collected(
        "allow_deny_batch",
        model,
        tools,
        Arc::new(authorizer),
        "Read and then try to write the demo file.",
    )
    .await?;

    let completed = evidence
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCompleted { status, .. } => Some(*status),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure(
        completed == [ToolCompletionStatus::Success, ToolCompletionStatus::Failed],
        "allow_deny_batch must expose one allowed and one denied result",
    )?;
    ensure(
        log.entries().contains(&LogEntry::ToolExecute {
            name: "read_demo".to_owned(),
        }),
        "allow_deny_batch did not execute the allowed tool",
    )?;
    ensure(
        !log.entries().contains(&LogEntry::ToolExecute {
            name: "write_demo".to_owned(),
        }),
        "allow_deny_batch executed the denied tool",
    )?;
    ensure(
        evidence.report.journal_roles
            == [
                MessageRole::User,
                MessageRole::Assistant,
                MessageRole::Tool,
                MessageRole::Tool,
                MessageRole::Assistant,
            ],
        "allow_deny_batch did not atomically project the whole result batch",
    )?;
    Ok(evidence.report)
}

async fn run_controlled_failure() -> Result<ScenarioReport, HarnessError> {
    let establishment_model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [ModelScript::FailEstablishment(ModelError::Transport {
            kind: ModelTransportErrorKind::Connection,
            message: "scripted offline outage".to_owned(),
        })],
    ));
    let mut establishment = execute_collected(
        "controlled_failure",
        establishment_model,
        ToolSetSnapshot::default(),
        Arc::new(agent_core::AllowAllAuthorizer),
        "Trigger a controlled model failure.",
    )
    .await?;
    ensure(
        matches!(
            establishment.outcome,
            ExecutionOutcome::Failed(ExecutionError::Model(ModelError::Transport { .. }))
        ),
        "controlled_failure establishment subcase did not fail as expected",
    )?;
    ensure(
        establishment.report.terminal == RunStatus::Failed,
        "controlled_failure primary run must be Failed",
    )?;

    let tool_turn = calls_message(
        "failure_tool_turn",
        vec![tool_call("failure_call", "explode_demo", json!({}))?],
    )?;
    let recovered_message =
        text_message("failure_recovered", "The tool error was handled as data.")?;
    let tool_model = Arc::new(ScriptedModelService::new(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        [
            ModelScript::Events(message_events(&tool_turn)),
            ModelScript::Events(message_events(&recovered_message)),
        ],
    ));
    let log = OrderLog::new();
    let tool_evidence = execute_collected(
        "controlled_failure_tool_error",
        tool_model,
        tool_snapshot(vec![ScriptedTool::failing(
            "explode_demo",
            "scripted tool failure",
            log.clone(),
        )])?,
        Arc::new(ScriptedAuthorizer::allow_all(log)),
        "Call the failing tool and continue.",
    )
    .await?;
    ensure(
        matches!(tool_evidence.outcome, ExecutionOutcome::Completed(_)),
        "controlled_failure tool error subcase did not continue",
    )?;
    ensure(
        tool_evidence.events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::ToolCompleted {
                    status: ToolCompletionStatus::Failed,
                    ..
                }
            )
        }),
        "controlled_failure tool error was not visible as ToolCompleted(Failed)",
    )?;
    establishment
        .report
        .event_summary
        .push("subcase.tool_error=completed_with_error_result".to_owned());
    establishment.report.event_summary.push(format!(
        "subcase.tool_error.roles={}",
        roles_text(&tool_evidence.report.journal_roles)
    ));
    Ok(establishment.report)
}

async fn run_cancelled() -> Result<ScenarioReport, HarnessError> {
    let mut runtime = HarnessRuntime::for_scenario("cancelled");
    let prepared = runtime.prepare_run("Wait until this run is cancelled.")?;
    let run_id = prepared.run_id.clone();
    let model = Arc::new(PausableModel::new()?);
    let started = model.started_signal();
    let execution = AgentExecution::start(
        make_spec(model, ToolSetSnapshot::default()),
        prepared.input,
        ExecutionContext {
            cancellation: CancellationToken::new(),
            recorder: prepared.recorder,
            authorizer: Arc::new(agent_core::AllowAllAuthorizer),
        },
    );
    runtime.mark_running(&run_id)?;
    runtime.attach_control(&run_id, execution.control.clone())?;
    let AgentExecution {
        events,
        completion,
        control: _,
    } = execution;
    let collector = tokio::spawn(events.collect::<Vec<_>>());
    started.notified().await;
    runtime.cancel_active()?;
    let outcome = completion.await;
    let events = collector
        .await
        .map_err(|error| HarnessError::Execution(format!("event collector failed: {error}")))?;
    observe_all(&mut runtime, &run_id, &events)?;
    runtime.finish_run(&run_id, outcome.clone())?;
    let report = build_report("cancelled", run_id, runtime.snapshot()?, &events, &outcome)?;

    ensure(
        outcome == ExecutionOutcome::Cancelled,
        "cancelled scenario did not converge to Cancelled",
    )?;
    ensure(
        report.journal_roles == [MessageRole::User],
        "cancelled model wait must not synthesize an assistant message",
    )?;
    ensure_lifecycle(&events)?;
    Ok(report)
}

async fn run_observation_disconnect() -> Result<ScenarioReport, HarnessError> {
    let mut runtime = HarnessRuntime::for_scenario("observation_disconnect");
    let prepared = runtime.prepare_run("Complete even without an event subscriber.")?;
    let run_id = prepared.run_id.clone();
    let final_message = text_message("disconnect_final", "Completion survived disconnect.")?;
    let model = Arc::new(ScriptedModelService::completing(
        capabilities(),
        TEST_CONTEXT_WINDOW_TOKENS,
        final_message.clone(),
    ));
    let execution = AgentExecution::start(
        make_spec(model, ToolSetSnapshot::default()),
        prepared.input,
        ExecutionContext {
            cancellation: CancellationToken::new(),
            recorder: prepared.recorder,
            authorizer: Arc::new(agent_core::AllowAllAuthorizer),
        },
    );
    runtime.mark_running(&run_id)?;
    runtime.attach_control(&run_id, execution.control.clone())?;
    let AgentExecution {
        events,
        completion,
        control: _,
    } = execution;
    drop(events);
    let outcome = completion.await;
    runtime.finish_run(&run_id, outcome.clone())?;
    let snapshot = runtime.snapshot()?;
    let run = snapshot
        .run
        .as_ref()
        .ok_or_else(|| scenario_error("observation_disconnect lost its Run snapshot"))?;
    ensure(
        outcome == ExecutionOutcome::Completed(final_message),
        "observation_disconnect changed the completion result",
    )?;
    ensure(
        run.event_count == 0 && run.terminal == Some(TerminalKind::Completed),
        "observation_disconnect did not infer terminal state from completion",
    )?;
    let mut report = build_report("observation_disconnect", run_id, snapshot, &[], &outcome)?;
    report
        .event_summary
        .push("event_stream=dropped_before_consumption".to_owned());
    report
        .event_summary
        .push("completion=unaffected".to_owned());
    Ok(report)
}

async fn execute_collected(
    name: &'static str,
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
    authorizer: Arc<dyn ToolAuthorizer>,
    user_text: &str,
) -> Result<ExecutionEvidence, HarnessError> {
    let mut runtime = HarnessRuntime::for_scenario(name);
    let prepared = runtime.prepare_run(user_text)?;
    let run_id = prepared.run_id.clone();
    let execution = AgentExecution::start(
        make_spec(model, tools),
        prepared.input,
        ExecutionContext {
            cancellation: CancellationToken::new(),
            recorder: prepared.recorder,
            authorizer,
        },
    );
    runtime.mark_running(&run_id)?;
    runtime.attach_control(&run_id, execution.control.clone())?;
    let AgentExecution {
        events,
        completion,
        control: _,
    } = execution;
    let collector = tokio::spawn(events.collect::<Vec<_>>());
    let outcome = completion.await;
    let events = collector
        .await
        .map_err(|error| HarnessError::Execution(format!("event collector failed: {error}")))?;
    ensure_lifecycle(&events)?;
    observe_all(&mut runtime, &run_id, &events)?;
    runtime.finish_run(&run_id, outcome.clone())?;
    let report = build_report(name, run_id, runtime.snapshot()?, &events, &outcome)?;
    Ok(ExecutionEvidence {
        report,
        outcome,
        events,
    })
}

fn make_spec(model: Arc<dyn ModelService>, tools: ToolSetSnapshot) -> ExecutionSpec {
    ExecutionSpec {
        system_prompt: SystemPromptSnapshot::new(vec![
            "This is a deterministic offline verification scenario.".to_owned(),
        ]),
        model,
        context_window: Arc::new(
            ContextWindowEvaluator::new(0.8).expect("valid scenario threshold"),
        ),
        tools,
        budget: ExecutionBudget {
            max_steps: Some(8),
            max_tool_calls: Some(8),
        },
        guardrails: None,
    }
}

fn build_report(
    name: &'static str,
    run_id: HarnessRunId,
    snapshot: RuntimeSnapshot,
    events: &[AgentEvent],
    outcome: &ExecutionOutcome,
) -> Result<ScenarioReport, HarnessError> {
    let run = snapshot
        .run
        .ok_or_else(|| scenario_error(format!("{name} lost its Run snapshot")))?;
    ensure(
        run.id == run_id,
        format!("{name} reported the wrong Run ID"),
    )?;
    let expected_status = match outcome {
        ExecutionOutcome::Completed(_) => RunStatus::Completed,
        ExecutionOutcome::Failed(_) => RunStatus::Failed,
        ExecutionOutcome::Cancelled => RunStatus::Cancelled,
        ExecutionOutcome::CompactionRequired { .. } => RunStatus::CompactionRequired,
    };
    ensure(
        run.status == expected_status,
        format!("{name} Run status disagrees with its completion outcome"),
    )?;
    ensure(
        snapshot.pending.is_empty(),
        format!("{name} left an incomplete tool exchange"),
    )?;
    let mut event_summary = summarize_events(events);
    event_summary.push(format!("outcome={}", format_outcome(outcome)));
    event_summary.push(format!("dropped_events={}", run.dropped_events));
    Ok(ScenarioReport {
        name,
        status: ScenarioStatus::Passed,
        run_id,
        terminal: run.status,
        event_summary,
        journal_roles: snapshot.roles,
        pending_count: snapshot.pending.len(),
    })
}

fn summarize_events(events: &[AgentEvent]) -> Vec<String> {
    let mut summary = Vec::new();
    if let Some(first) = events.first() {
        summary.push(format_event(first));
    }
    summary.push(format!("steps={}", count_steps(events)));
    summary.push(format!(
        "tool_completed={}",
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolCompleted { .. }))
            .count()
    ));
    if let Some(terminal) = events.last().filter(|event| event.is_terminal()) {
        summary.push(format_event(terminal));
    }
    summary
}

fn observe_all(
    runtime: &mut HarnessRuntime,
    run_id: &HarnessRunId,
    events: &[AgentEvent],
) -> Result<(), HarnessError> {
    for event in events {
        runtime.observe_event(run_id, event)?;
    }
    Ok(())
}

fn ensure_lifecycle(events: &[AgentEvent]) -> Result<(), HarnessError> {
    ensure(
        matches!(events.first(), Some(AgentEvent::ExecutionStarted)),
        "first Agent event must be ExecutionStarted",
    )?;
    ensure(
        events.iter().filter(|event| event.is_terminal()).count() == 1,
        "Agent event stream must contain exactly one terminal",
    )?;
    ensure(
        events.last().is_some_and(AgentEvent::is_terminal),
        "last Agent event must be terminal",
    )
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        reasoning: true,
        tool_calls: true,
        streaming: true,
    }
}

fn text_message(id: &str, text: &str) -> Result<AssistantMessage, HarnessError> {
    Ok(AssistantMessage {
        id: message_id(id)?,
        model: model_identity()?,
        parts: vec![AssistantPart::Text(TextPart {
            id: part_id(&format!("{id}_text"))?,
            text: text.to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    })
}

fn calls_message(id: &str, calls: Vec<ToolCall>) -> Result<AssistantMessage, HarnessError> {
    Ok(AssistantMessage {
        id: message_id(id)?,
        model: model_identity()?,
        parts: calls.into_iter().map(AssistantPart::ToolCall).collect(),
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    })
}

fn tool_call(id: &str, name: &str, arguments: Value) -> Result<ToolCall, HarnessError> {
    Ok(ToolCall {
        id: ToolCallId::new(id).map_err(|error| scenario_error(error.to_string()))?,
        name: ToolName::new(name).map_err(|error| scenario_error(error.to_string()))?,
        arguments,
    })
}

fn message_id(value: &str) -> Result<MessageId, HarnessError> {
    MessageId::new(value).map_err(|error| scenario_error(error.to_string()))
}

fn part_id(value: &str) -> Result<PartId, HarnessError> {
    PartId::new(value).map_err(|error| scenario_error(error.to_string()))
}

fn model_identity() -> Result<ModelIdentity, HarnessError> {
    Ok(ModelIdentity::new(
        ProviderId::new("offline").map_err(|error| scenario_error(error.to_string()))?,
        "scripted-v0.2",
    ))
}

fn tool_snapshot(tools: Vec<ScriptedTool>) -> Result<ToolSetSnapshot, HarnessError> {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry
            .register(tool)
            .map_err(|error| HarnessError::ToolRegistration(error.to_string()))?;
    }
    Ok(registry.snapshot())
}

fn count_steps(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, AgentEvent::StepStarted { .. }))
        .count()
}

fn roles_text(roles: &[MessageRole]) -> String {
    roles
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("->")
}

fn ensure(condition: bool, message: impl Into<String>) -> Result<(), HarnessError> {
    if condition {
        Ok(())
    } else {
        Err(scenario_error(message))
    }
}

fn scenario_error(message: impl Into<String>) -> HarnessError {
    HarnessError::ScenarioFailed(message.into())
}

struct PausableModel {
    capabilities: ModelCapabilities,
    started: Arc<Notify>,
    message_id: MessageId,
    identity: ModelIdentity,
}

impl PausableModel {
    fn new() -> Result<Self, HarnessError> {
        Ok(Self {
            capabilities: capabilities(),
            started: Arc::new(Notify::new()),
            message_id: message_id("cancelled_turn")?,
            identity: model_identity()?,
        })
    }

    fn started_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.started)
    }
}

impl ModelService for PausableModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        TEST_CONTEXT_WINDOW_TOKENS
    }

    fn stream(&self, _request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        let started = Arc::clone(&self.started);
        let cancellation = context.cancellation;
        let first = ModelEvent::TurnStarted {
            message_id: self.message_id.clone(),
            model: self.identity.clone(),
        };
        Box::pin(async move {
            let tail = stream::once(async move {
                started.notify_one();
                cancellation.cancelled().await;
                ModelEvent::TurnFailed {
                    error: ModelError::Cancelled,
                }
            });
            Ok(Box::pin(stream::iter([first]).chain(tail)) as ModelEventStream)
        })
    }
}
