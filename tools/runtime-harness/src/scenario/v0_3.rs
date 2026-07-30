//! Cumulative offline scenarios for the v0.3 context capability baseline.

use std::sync::Arc;

use agent_context::{
    CompressionStrategy, ContextWindowDecision, ContextWindowEvaluator, RollingSummaryPolicy,
    RollingSummarySameModel,
};
use agent_core::{
    AgentEvent, AgentExecution, CompactionReason, ExecutionBudget, ExecutionOutcome, ExecutionSpec,
};
use agent_model::{ModelCapabilities, ModelError, ModelEvent, ModelService};
use agent_testkit::{ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events};
use agent_tools::{ToolRegistry, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, PartId, ProviderId,
    TextPart, TokenUsage, ToolCall, ToolCallId, ToolName,
};
use futures_util::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    HarnessError,
    context::{HarnessCompactionCause, HarnessCompactionOutcome},
    output::{format_event, format_outcome},
    runtime::{HarnessRunId, HarnessRuntime, MessageRole, PreparedRun, RuntimeSnapshot},
    scenario::{ScenarioFuture, ScenarioReport, ScenarioStatus},
};

const WINDOW_TOKENS: u64 = 100;
const THRESHOLD_RATIO: f64 = 0.8;

struct RunEvidence {
    run_id: HarnessRunId,
    outcome: ExecutionOutcome,
    events: Vec<AgentEvent>,
}

pub(super) fn context_short_path() -> ScenarioFuture {
    Box::pin(run_context_short_path())
}

pub(super) fn context_before_run_compaction() -> ScenarioFuture {
    Box::pin(run_context_before_run_compaction())
}

pub(super) fn context_in_run_continuation() -> ScenarioFuture {
    Box::pin(run_context_in_run_continuation())
}

pub(super) fn context_provider_overflow_recovery() -> ScenarioFuture {
    Box::pin(run_context_provider_overflow_recovery())
}

pub(super) fn context_user_compaction() -> ScenarioFuture {
    Box::pin(run_context_user_compaction())
}

pub(super) fn context_rolling_checkpoints() -> ScenarioFuture {
    Box::pin(run_context_rolling_checkpoints())
}

pub(super) fn context_queued_compaction() -> ScenarioFuture {
    Box::pin(run_context_queued_compaction())
}

pub(super) fn context_failure_boundaries() -> ScenarioFuture {
    Box::pin(run_context_failure_boundaries())
}

async fn run_context_short_path() -> Result<ScenarioReport, HarnessError> {
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [events(text_message("short_final", "short path", 20)?)],
    ));
    let service: Arc<dyn ModelService> = model.clone();
    let mut runtime = runtime("context_short_path", 2);
    let prepared = prepare_initial(
        &mut runtime,
        "A short request.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    ensure(
        prepared.preflight.as_ref().is_some_and(|evaluation| {
            evaluation.decision == ContextWindowDecision::UsageUnavailable
        }),
        "short path must report UsageUnavailable before the first model result",
    )?;
    ensure(prepared.compaction.is_none(), "short path must not compact")?;
    let evidence =
        execute_prepared(&mut runtime, prepared, service, ToolSetSnapshot::default()).await?;
    ensure(
        matches!(evidence.outcome, ExecutionOutcome::Completed(_)),
        "short path did not complete",
    )?;
    ensure(
        runtime.snapshot()?.checkpoint_count == 0,
        "short path unexpectedly committed a checkpoint",
    )?;
    ensure(
        model.take_requests().len() == 1,
        "short path must make exactly one model request",
    )?;
    build_report(
        "context_short_path",
        &runtime,
        evidence.run_id,
        &evidence.events,
        &evidence.outcome,
        vec!["context=ready_without_compaction".to_owned()],
    )
}

async fn run_context_before_run_compaction() -> Result<ScenarioReport, HarnessError> {
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("before_seed", "seed", 90)?),
            events(text_message("before_summary", "summary", 15)?),
            events(text_message("before_final", "after compaction", 30)?),
        ],
    ));
    let service: Arc<dyn ModelService> = model.clone();
    let mut runtime = runtime("context_before_run_compaction", 2);
    let seed = prepare_initial(
        &mut runtime,
        "Seed a large completed result.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(
        &mut runtime,
        seed,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;

    let prepared = prepare_initial(
        &mut runtime,
        "Start after the threshold.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let report = prepared
        .compaction
        .as_ref()
        .ok_or_else(|| scenario_error("before-run path did not compact"))?;
    ensure(
        report.cause == HarnessCompactionCause::BeforeRunThreshold,
        "before-run path recorded the wrong cause",
    )?;
    ensure(
        runtime.snapshot()?.checkpoint_count == 1,
        "before-run compaction did not commit before Run creation",
    )?;
    let evidence =
        execute_prepared(&mut runtime, prepared, service, ToolSetSnapshot::default()).await?;
    let requests = model.take_requests();
    ensure(
        requests.len() == 3 && requests[1].tools.is_empty(),
        "before-run compaction did not use one tool-free compression request",
    )?;
    build_report(
        "context_before_run_compaction",
        &runtime,
        evidence.run_id,
        &evidence.events,
        &evidence.outcome,
        vec!["checkpoint=committed_before_initial_run".to_owned()],
    )
}

async fn run_context_in_run_continuation() -> Result<ScenarioReport, HarnessError> {
    let tool_turn = tool_message("in_run_tool", "in_run_call", 90)?;
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("in_run_seed", "seed", 20)?),
            events(tool_turn),
            events(text_message("in_run_summary", "summary", 15)?),
            events(text_message("in_run_final", "continued", 25)?),
        ],
    ));
    let service: Arc<dyn ModelService> = model.clone();
    let tools = tool_snapshot()?;
    let mut runtime = runtime("context_in_run_continuation", 2);
    let seed = prepare_initial(
        &mut runtime,
        "Seed an older turn.",
        Arc::clone(&service),
        tools.clone(),
    )
    .await?;
    execute_prepared(&mut runtime, seed, Arc::clone(&service), tools.clone()).await?;

    let prepared = prepare_initial(
        &mut runtime,
        "Use the scripted tool.",
        Arc::clone(&service),
        tools.clone(),
    )
    .await?;
    let interrupted =
        execute_prepared(&mut runtime, prepared, Arc::clone(&service), tools.clone()).await?;
    let (reason, step) = compaction_required(&interrupted.outcome)?;
    ensure(
        reason == CompactionReason::ThresholdReached && step == 2,
        "tool loop did not hand off at the second model step",
    )?;
    let continuation = runtime
        .prepare_continuation(
            interrupted.run_id.clone(),
            reason,
            step,
            Arc::new(spec(Arc::clone(&service), tools.clone())),
            strategy(),
            CancellationToken::new(),
        )
        .await?;
    ensure(
        continuation.user.is_none(),
        "continuation duplicated the user message",
    )?;
    let evidence = execute_prepared(&mut runtime, continuation, service, tools).await?;
    ensure(
        runtime.snapshot()?.checkpoint_count == 1,
        "in-run continuation did not commit a checkpoint",
    )?;
    build_report(
        "context_in_run_continuation",
        &runtime,
        evidence.run_id,
        &evidence.events,
        &evidence.outcome,
        vec![format!("continuation_of={}", interrupted.run_id)],
    )
}

async fn run_context_provider_overflow_recovery() -> Result<ScenarioReport, HarnessError> {
    let establishment = overflow_subcase(
        "overflow_establishment",
        ModelScript::FailEstablishment(overflow_error()),
    )
    .await?;
    ensure(
        establishment.1 == CompactionReason::ProviderOverflow,
        "establishment overflow used the wrong reason",
    )?;
    let stream = overflow_subcase(
        "overflow_stream",
        ModelScript::Events(vec![
            ModelEvent::TurnStarted {
                message_id: message_id("overflow_stream_turn")?,
                model: identity()?,
            },
            ModelEvent::TurnFailed {
                error: overflow_error(),
            },
        ]),
    )
    .await?;
    ensure(
        stream.1 == CompactionReason::ProviderOverflow,
        "stream overflow used the wrong reason",
    )?;
    let mut report = stream.0;
    report.name = "context_provider_overflow_recovery";
    report
        .event_summary
        .push("establishment_overflow=recovered".to_owned());
    report
        .event_summary
        .push("stream_overflow=recovered".to_owned());
    Ok(report)
}

async fn run_context_user_compaction() -> Result<ScenarioReport, HarnessError> {
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("user_compact_first", "first", 20)?),
            events(text_message("user_compact_second", "second", 25)?),
            events(text_message("user_compact_summary", "summary", 15)?),
        ],
    ));
    let service: Arc<dyn ModelService> = model.clone();
    let mut runtime = runtime("context_user_compaction", 2);
    let first = prepare_initial(
        &mut runtime,
        "First turn.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(
        &mut runtime,
        first,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let second = prepare_initial(
        &mut runtime,
        "Second turn.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let second_evidence = execute_prepared(
        &mut runtime,
        second,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let next_run = runtime.next_run_id();
    let outcome = runtime
        .compact_user_context(
            Arc::new(spec(Arc::clone(&service), ToolSetSnapshot::default())),
            strategy(),
            CancellationToken::new(),
        )
        .await?;
    ensure(
        matches!(outcome, HarnessCompactionOutcome::Compacted { .. }),
        "user compaction did not produce a checkpoint",
    )?;
    ensure(
        runtime.next_run_id() == next_run,
        "user compaction allocated or continued a Run",
    )?;
    ensure(
        runtime.snapshot()?.checkpoint_count == 1,
        "user compaction did not commit exactly one checkpoint",
    )?;
    build_report(
        "context_user_compaction",
        &runtime,
        second_evidence.run_id,
        &second_evidence.events,
        &second_evidence.outcome,
        vec!["user_compaction=completed_without_run".to_owned()],
    )
}

async fn run_context_rolling_checkpoints() -> Result<ScenarioReport, HarnessError> {
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("rolling_first", "first", 90)?),
            events(text_message("rolling_summary_one", "summary one", 15)?),
            events(text_message("rolling_second", "second", 90)?),
            events(text_message("rolling_summary_two", "summary two", 15)?),
            events(text_message("rolling_third", "third", 25)?),
        ],
    ));
    let service: Arc<dyn ModelService> = model.clone();
    let mut runtime = runtime("context_rolling_checkpoints", 2);
    let first = prepare_initial(
        &mut runtime,
        "First rolling turn.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(
        &mut runtime,
        first,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let second = prepare_initial(
        &mut runtime,
        "Second rolling turn.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(
        &mut runtime,
        second,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let third = prepare_initial(
        &mut runtime,
        "Third rolling turn.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let evidence =
        execute_prepared(&mut runtime, third, service, ToolSetSnapshot::default()).await?;
    let snapshot = runtime.snapshot()?;
    ensure(
        snapshot.checkpoint_count == 2,
        "rolling compaction did not retain both checkpoint records",
    )?;
    ensure(
        snapshot.effective_roles.first() == Some(&MessageRole::ContextSummary),
        "latest checkpoint projection does not start from the rolling summary",
    )?;
    ensure(
        snapshot
            .roles
            .iter()
            .filter(|role| **role == MessageRole::User)
            .count()
            == 3,
        "rolling checkpoints hid original user messages",
    )?;
    build_report(
        "context_rolling_checkpoints",
        &runtime,
        evidence.run_id,
        &evidence.events,
        &evidence.outcome,
        vec!["rolling_checkpoints=2".to_owned()],
    )
}

async fn run_context_queued_compaction() -> Result<ScenarioReport, HarnessError> {
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("queued_first", "first", 20)?),
            events(text_message("queued_second", "second", 25)?),
            events(text_message("queued_summary", "summary", 15)?),
        ],
    ));
    let service: Arc<dyn ModelService> = model.clone();
    let mut runtime = runtime("context_queued_compaction", 2);
    let first = prepare_initial(
        &mut runtime,
        "First queued turn.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(
        &mut runtime,
        first,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let second = prepare_initial(
        &mut runtime,
        "Second queued turn.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    runtime.queue_user_compaction();
    ensure(
        runtime.snapshot()?.user_compaction_queued,
        "active task did not retain the queued compaction",
    )?;
    let evidence = execute_prepared(
        &mut runtime,
        second,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    ensure(
        runtime.take_queued_user_compaction(),
        "queued compaction disappeared before task completion",
    )?;
    let outcome = runtime
        .compact_user_context(
            Arc::new(spec(service, ToolSetSnapshot::default())),
            strategy(),
            CancellationToken::new(),
        )
        .await?;
    ensure(
        matches!(outcome, HarnessCompactionOutcome::Compacted { .. }),
        "queued compaction did not run after the task",
    )?;
    build_report(
        "context_queued_compaction",
        &runtime,
        evidence.run_id,
        &evidence.events,
        &evidence.outcome,
        vec!["queued_compaction=serialized_after_task".to_owned()],
    )
}

async fn run_context_failure_boundaries() -> Result<ScenarioReport, HarnessError> {
    let noop_model = Arc::new(ScriptedModelService::new(capabilities(), WINDOW_TOKENS, []));
    let noop_service: Arc<dyn ModelService> = noop_model.clone();
    let mut noop_runtime = runtime("failure_noop", 1);
    let noop = noop_runtime
        .compact_user_context(
            Arc::new(spec(noop_service, ToolSetSnapshot::default())),
            strategy(),
            CancellationToken::new(),
        )
        .await?;
    ensure(
        matches!(noop, HarnessCompactionOutcome::NoOp { .. })
            && noop_model.take_requests().is_empty(),
        "no-head compaction must be a no-request NoOp",
    )?;

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let cancelled_model: Arc<dyn ModelService> =
        Arc::new(ScriptedModelService::new(capabilities(), WINDOW_TOKENS, []));
    let cancelled_error = match noop_runtime
        .compact_user_context(
            Arc::new(spec(cancelled_model, ToolSetSnapshot::default())),
            strategy(),
            cancelled,
        )
        .await
    {
        Ok(_) => {
            return Err(scenario_error(
                "pre-cancelled compaction unexpectedly succeeded",
            ));
        }
        Err(error) => error,
    };
    ensure(
        cancelled_error.to_string().contains("cancelled"),
        "pre-cancelled compaction lost its classification",
    )?;

    let invalid_model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("invalid_first", "first", 20)?),
            events(text_message("invalid_second", "second", 20)?),
            events(message_with_reason(
                "invalid_summary",
                "invalid",
                FinishReason::Length,
                10,
            )?),
        ],
    ));
    let invalid_service: Arc<dyn ModelService> = invalid_model.clone();
    let mut invalid_runtime = runtime("failure_invalid", 1);
    complete_two_turns(&mut invalid_runtime, Arc::clone(&invalid_service)).await?;
    let invalid_error = match invalid_runtime
        .compact_user_context(
            Arc::new(spec(
                Arc::clone(&invalid_service),
                ToolSetSnapshot::default(),
            )),
            strategy(),
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => return Err(scenario_error("invalid summary unexpectedly committed")),
        Err(error) => error,
    };
    ensure(
        invalid_error
            .to_string()
            .contains("invalid compaction response")
            && invalid_runtime.snapshot()?.checkpoint_count == 0,
        "invalid summary produced a checkpoint",
    )?;

    let store_model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("store_first", "first", 20)?),
            events(text_message("store_second", "second", 20)?),
            events(text_message("store_summary", "summary", 10)?),
        ],
    ));
    let store_service: Arc<dyn ModelService> = store_model.clone();
    let mut store_runtime = runtime("failure_store", 1);
    let last = complete_two_turns(&mut store_runtime, Arc::clone(&store_service)).await?;
    store_runtime.inject_checkpoint_failure_for_verification();
    let store_error = match store_runtime
        .compact_user_context(
            Arc::new(spec(store_service, ToolSetSnapshot::default())),
            strategy(),
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => {
            return Err(scenario_error(
                "injected checkpoint failure unexpectedly committed",
            ));
        }
        Err(error) => error,
    };
    ensure(
        store_error.to_string().contains("checkpoint commit")
            && store_runtime.snapshot()?.checkpoint_count == 0,
        "checkpoint failure left a partial checkpoint",
    )?;

    let limit_model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message("limit_seed", "seed", 90)?),
            events(text_message("limit_summary", "summary", 15)?),
            events(tool_message("limit_tool", "limit_call", 90)?),
        ],
    ));
    let limit_service: Arc<dyn ModelService> = limit_model.clone();
    let limit_tools = tool_snapshot()?;
    let mut limit_runtime = runtime("failure_limit", 1);
    let seed = prepare_initial(
        &mut limit_runtime,
        "Seed a high-usage turn.",
        Arc::clone(&limit_service),
        limit_tools.clone(),
    )
    .await?;
    execute_prepared(
        &mut limit_runtime,
        seed,
        Arc::clone(&limit_service),
        limit_tools.clone(),
    )
    .await?;
    let limited = prepare_initial(
        &mut limit_runtime,
        "Use one automatic compaction, then request another.",
        Arc::clone(&limit_service),
        limit_tools.clone(),
    )
    .await?;
    let interrupted = execute_prepared(
        &mut limit_runtime,
        limited,
        Arc::clone(&limit_service),
        limit_tools.clone(),
    )
    .await?;
    let (reason, step) = compaction_required(&interrupted.outcome)?;
    let limit_error = match limit_runtime
        .prepare_continuation(
            interrupted.run_id,
            reason,
            step,
            Arc::new(spec(limit_service, limit_tools)),
            strategy(),
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => {
            return Err(scenario_error(
                "automatic compaction limit unexpectedly allowed continuation",
            ));
        }
        Err(error) => error,
    };
    ensure(
        limit_error
            .to_string()
            .contains("automatic harness context compaction limit reached")
            && limit_model.take_requests().len() == 3,
        "automatic compaction limit invoked another strategy request",
    )?;

    let report = build_report(
        "context_failure_boundaries",
        &store_runtime,
        last.run_id,
        &last.events,
        &last.outcome,
        vec![
            "no_head=no_op".to_owned(),
            "invalid_summary=rejected".to_owned(),
            "cancelled=stopped".to_owned(),
            "checkpoint_failure=atomic".to_owned(),
            "automatic_limit=stopped_before_strategy".to_owned(),
        ],
    )?;
    Ok(report)
}

async fn overflow_subcase(
    name: &'static str,
    overflow_script: ModelScript,
) -> Result<(ScenarioReport, CompactionReason), HarnessError> {
    let model = Arc::new(ScriptedModelService::new(
        capabilities(),
        WINDOW_TOKENS,
        [
            events(text_message(&format!("{name}_seed"), "seed", 20)?),
            overflow_script,
            events(text_message(&format!("{name}_summary"), "summary", 15)?),
            events(text_message(&format!("{name}_final"), "recovered", 25)?),
        ],
    ));
    let service: Arc<dyn ModelService> = model;
    let mut runtime = runtime(name, 2);
    let seed = prepare_initial(
        &mut runtime,
        "Seed older history.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(
        &mut runtime,
        seed,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let overflow = prepare_initial(
        &mut runtime,
        "Trigger provider overflow.",
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let interrupted = execute_prepared(
        &mut runtime,
        overflow,
        Arc::clone(&service),
        ToolSetSnapshot::default(),
    )
    .await?;
    let (reason, step) = compaction_required(&interrupted.outcome)?;
    let continuation = runtime
        .prepare_continuation(
            interrupted.run_id.clone(),
            reason,
            step,
            Arc::new(spec(Arc::clone(&service), ToolSetSnapshot::default())),
            strategy(),
            CancellationToken::new(),
        )
        .await?;
    let evidence = execute_prepared(
        &mut runtime,
        continuation,
        service,
        ToolSetSnapshot::default(),
    )
    .await?;
    let report = build_report(
        name,
        &runtime,
        evidence.run_id,
        &evidence.events,
        &evidence.outcome,
        vec![format!("overflow_continuation_of={}", interrupted.run_id)],
    )?;
    Ok((report, reason))
}

async fn complete_two_turns(
    runtime: &mut HarnessRuntime,
    model: Arc<dyn ModelService>,
) -> Result<RunEvidence, HarnessError> {
    let first = prepare_initial(
        runtime,
        "First completed turn.",
        Arc::clone(&model),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(
        runtime,
        first,
        Arc::clone(&model),
        ToolSetSnapshot::default(),
    )
    .await?;
    let second = prepare_initial(
        runtime,
        "Second completed turn.",
        Arc::clone(&model),
        ToolSetSnapshot::default(),
    )
    .await?;
    execute_prepared(runtime, second, model, ToolSetSnapshot::default()).await
}

async fn prepare_initial(
    runtime: &mut HarnessRuntime,
    text: &str,
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
) -> Result<PreparedRun, HarnessError> {
    runtime
        .prepare_context_run(
            text,
            Arc::new(spec(model, tools)),
            strategy(),
            CancellationToken::new(),
        )
        .await
}

async fn execute_prepared(
    runtime: &mut HarnessRuntime,
    prepared: PreparedRun,
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
) -> Result<RunEvidence, HarnessError> {
    let (run_id, execution) = runtime.start_prepared(spec(model, tools), prepared)?;
    let AgentExecution {
        events,
        completion,
        control: _,
    } = execution;
    let events = events.collect::<Vec<_>>().await;
    let outcome = completion.await;
    for event in &events {
        runtime.observe_event(&run_id, event)?;
    }
    runtime.finish_run(&run_id, outcome.clone())?;
    Ok(RunEvidence {
        run_id,
        outcome,
        events,
    })
}

fn build_report(
    name: &'static str,
    runtime: &HarnessRuntime,
    run_id: HarnessRunId,
    events: &[AgentEvent],
    outcome: &ExecutionOutcome,
    mut extra: Vec<String>,
) -> Result<ScenarioReport, HarnessError> {
    let RuntimeSnapshot {
        run,
        roles,
        pending,
        checkpoint_count,
        automatic_compactions,
        ..
    } = runtime.snapshot()?;
    let run = run.ok_or_else(|| scenario_error(format!("{name} lost its final Run")))?;
    ensure(run.id == run_id, format!("{name} reported the wrong Run"))?;
    ensure(
        pending.is_empty(),
        format!("{name} left a pending tool exchange"),
    )?;
    let mut event_summary = vec![
        format!("outcome={}", format_outcome(outcome)),
        format!("checkpoints={checkpoint_count}"),
        format!("automatic_compactions={automatic_compactions}"),
    ];
    if let Some(first) = events.first() {
        event_summary.push(format_event(first));
    }
    if let Some(last) = events.last() {
        event_summary.push(format_event(last));
    }
    event_summary.append(&mut extra);
    Ok(ScenarioReport {
        name,
        status: ScenarioStatus::Passed,
        run_id,
        terminal: run.status,
        event_summary,
        journal_roles: roles,
        pending_count: pending.len(),
    })
}

fn runtime(name: &str, max_automatic_compactions: u32) -> HarnessRuntime {
    HarnessRuntime::with_session_id_and_limit(
        crate::runtime::HarnessSessionId::from_value(format!("session_verify_{name}")),
        max_automatic_compactions,
    )
}

fn spec(model: Arc<dyn ModelService>, tools: ToolSetSnapshot) -> ExecutionSpec {
    ExecutionSpec {
        instructions: vec!["This is a deterministic v0.3 context scenario.".to_owned()],
        model,
        context_window: Arc::new(
            ContextWindowEvaluator::new(THRESHOLD_RATIO).expect("valid scenario threshold"),
        ),
        tools,
        budget: ExecutionBudget {
            max_steps: Some(8),
            max_tool_calls: Some(8),
        },
    }
}

fn strategy() -> Arc<dyn CompressionStrategy> {
    Arc::new(RollingSummarySameModel::new(
        RollingSummaryPolicy::new(32, 1).expect("valid scenario rolling policy"),
    ))
}

fn events(message: AssistantMessage) -> ModelScript {
    ModelScript::Events(message_events(&message))
}

fn text_message(id: &str, text: &str, total_tokens: u64) -> Result<AssistantMessage, HarnessError> {
    message_with_reason(id, text, FinishReason::Stop, total_tokens)
}

fn message_with_reason(
    id: &str,
    text: &str,
    finish_reason: FinishReason,
    total_tokens: u64,
) -> Result<AssistantMessage, HarnessError> {
    Ok(AssistantMessage {
        id: message_id(id)?,
        model: identity()?,
        parts: vec![AssistantPart::Text(TextPart {
            id: part_id(&format!("{id}_text"))?,
            text: text.to_owned(),
        })],
        finish_reason,
        usage: Some(usage(total_tokens)),
    })
}

fn tool_message(
    id: &str,
    call_id: &str,
    total_tokens: u64,
) -> Result<AssistantMessage, HarnessError> {
    Ok(AssistantMessage {
        id: message_id(id)?,
        model: identity()?,
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(call_id).map_err(|error| scenario_error(error.to_string()))?,
            name: ToolName::new("context_demo")
                .map_err(|error| scenario_error(error.to_string()))?,
            arguments: json!({}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: Some(usage(total_tokens)),
    })
}

fn tool_snapshot() -> Result<ToolSetSnapshot, HarnessError> {
    let mut registry = ToolRegistry::new();
    registry
        .register(ScriptedTool::succeed(
            "context_demo",
            json!({"ok": true}),
            OrderLog::new(),
        ))
        .map_err(|error| HarnessError::ToolRegistration(error.to_string()))?;
    Ok(registry.snapshot())
}

fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        reasoning: true,
        tool_calls: true,
        streaming: true,
    }
}

fn usage(total_tokens: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: total_tokens.saturating_sub(10),
        output_tokens: total_tokens.min(10),
        total_tokens,
        cached_input_tokens: None,
        reasoning_tokens: None,
    }
}

fn compaction_required(
    outcome: &ExecutionOutcome,
) -> Result<(CompactionReason, u32), HarnessError> {
    match outcome {
        ExecutionOutcome::CompactionRequired { reason, step } => Ok((*reason, *step)),
        other => Err(scenario_error(format!(
            "expected CompactionRequired, got {}",
            format_outcome(other)
        ))),
    }
}

fn overflow_error() -> ModelError {
    ModelError::ContextOverflow {
        message: "scripted context overflow".to_owned(),
    }
}

fn identity() -> Result<ModelIdentity, HarnessError> {
    Ok(ModelIdentity::new(
        ProviderId::new("offline").map_err(|error| scenario_error(error.to_string()))?,
        "scripted-v0.3",
    ))
}

fn message_id(value: &str) -> Result<MessageId, HarnessError> {
    MessageId::new(value).map_err(|error| scenario_error(error.to_string()))
}

fn part_id(value: &str) -> Result<PartId, HarnessError> {
    PartId::new(value).map_err(|error| scenario_error(error.to_string()))
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
