//! Stable console projections for events, runs, and journal snapshots.

use std::fmt::Write;

use agent_core::{AgentEvent, ExecutionOutcome};

use crate::{
    context::HarnessCompactionOutcome,
    runtime::RuntimeSnapshot,
    scenario::{ScenarioStatus, VerificationSummary, versions},
};

pub(crate) fn list_text() -> String {
    let mut output = String::from("runtime-harness versions\n");
    for version in versions() {
        let _ = writeln!(output, "{}", version.version);
        let _ = writeln!(output, "  capabilities:");
        for capability in version.capabilities {
            let _ = writeln!(output, "    - {capability}");
        }
        match version.offline_verify {
            Some(baseline) => {
                let scenario_count = crate::scenario::definitions(baseline).len();
                let _ = writeln!(
                    output,
                    "  offline: verify {baseline} ({scenario_count} scenarios)"
                );
            }
            None => {
                let _ = writeln!(output, "  offline: none");
            }
        }
        let _ = writeln!(output, "  manual: {}", version.manual_modes.join(", "));
    }
    output
}

pub(crate) fn format_verification(summary: &VerificationSummary) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "verify {} ({} scenarios)",
        summary.baseline,
        summary.results.len()
    );
    for result in &summary.results {
        match (&result.status, &result.report) {
            (ScenarioStatus::Passed, Some(report)) => {
                let roles = report
                    .journal_roles
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                let _ = writeln!(
                    output,
                    "[PASS] {} run={} terminal={} pending={}",
                    result.name, report.run_id, report.terminal, report.pending_count
                );
                let _ = writeln!(output, "       journal={roles}");
                let _ = writeln!(output, "       events={}", report.event_summary.join("; "));
            }
            _ => {
                let error = result.error.as_deref().unwrap_or("missing scenario report");
                let _ = writeln!(output, "[FAIL] {} error={error}", result.name);
            }
        }
    }
    let _ = write!(
        output,
        "summary: {} passed, {} failed",
        summary.passed(),
        summary.failed()
    );
    output
}

pub(crate) fn format_state(snapshot: &RuntimeSnapshot) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "session: {}", snapshot.session_id);
    if let Some(run) = &snapshot.run {
        let elapsed = run
            .elapsed
            .map(|duration| format!("{}ms", duration.as_millis()))
            .unwrap_or_else(|| "not-started".to_owned());
        let _ = writeln!(
            output,
            "run: {} status={} elapsed={elapsed}",
            run.id, run.status
        );
        let created = run
            .created_at_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_owned());
        let started = run
            .started_at_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_owned());
        let finished = run
            .finished_at_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_owned());
        let _ = writeln!(
            output,
            "wall_clock_ms: created={created} started={started} finished={finished}"
        );
        let terminal = run
            .terminal
            .map(|terminal| terminal.to_string())
            .unwrap_or_else(|| "none".to_owned());
        let _ = writeln!(
            output,
            "events: count={} terminal={terminal} dropped={}",
            run.event_count, run.dropped_events
        );
    } else {
        let _ = writeln!(output, "run: none");
    }

    let roles = snapshot
        .roles
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" -> ");
    let _ = writeln!(
        output,
        "journal: completed={} roles=[{roles}]",
        snapshot.completed_messages
    );
    let _ = writeln!(output, "pending: {}", snapshot.pending.len());
    for pending in &snapshot.pending {
        let _ = writeln!(
            output,
            "  {} assistant={}",
            pending.receipt, pending.assistant_message_id
        );
    }
    let effective_roles = snapshot
        .effective_roles
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" -> ");
    let _ = writeln!(
        output,
        "context: checkpoints={} effective_roles=[{effective_roles}]",
        snapshot.checkpoint_count
    );
    let _ = writeln!(
        output,
        "compactions: automatic={}/{} user_queued={}",
        snapshot.automatic_compactions,
        snapshot.max_automatic_compactions,
        snapshot.user_compaction_queued
    );
    if let Some(report) = &snapshot.last_compaction {
        let _ = writeln!(
            output,
            "last_compaction: cause={:?} strategy={} compressed_blocks={} retained_blocks={}",
            report.cause,
            report.strategy.strategy,
            report.strategy.compressed_blocks,
            report.strategy.retained_blocks
        );
    } else {
        let _ = writeln!(output, "last_compaction: none");
    }
    output
}

pub(crate) fn format_compaction_outcome(outcome: &HarnessCompactionOutcome) -> String {
    match outcome {
        HarnessCompactionOutcome::Compacted { report, .. } => format!(
            "compacted cause={:?} strategy={} compressed_blocks={} retained_blocks={}",
            report.cause,
            report.strategy.strategy,
            report.strategy.compressed_blocks,
            report.strategy.retained_blocks
        ),
        HarnessCompactionOutcome::NoOp { report } => format!(
            "no_op cause={:?} strategy={} compressed_blocks={} retained_blocks={}",
            report.cause,
            report.strategy.strategy,
            report.strategy.compressed_blocks,
            report.strategy.retained_blocks
        ),
    }
}

pub(crate) fn format_event(event: &AgentEvent) -> String {
    match event {
        AgentEvent::ExecutionStarted => "agent.execution_started".to_owned(),
        AgentEvent::UsageUpdated { step, usage } => format!(
            "agent.usage_updated step={step} input={} output={} total={}",
            usage.input_tokens, usage.output_tokens, usage.total_tokens
        ),
        AgentEvent::StepStarted { step } => format!("agent.step_started step={step}"),
        AgentEvent::TextDelta { step, id, delta } => {
            format!("agent.text_delta step={step} id={id} text={delta}")
        }
        AgentEvent::ReasoningDelta { step, id, delta } => {
            format!("agent.reasoning_delta step={step} id={id} text={delta}")
        }
        AgentEvent::ToolProposed { step, call } => {
            format!(
                "agent.tool_proposed step={step} id={} name={}",
                call.id, call.name
            )
        }
        AgentEvent::ToolStarted { step, call_id } => {
            format!("agent.tool_started step={step} id={call_id}")
        }
        AgentEvent::ToolOutput {
            step,
            call_id,
            channel,
            chunk,
        } => {
            format!("agent.tool_output step={step} id={call_id} channel={channel:?} text={chunk}")
        }
        AgentEvent::ToolCompleted {
            step,
            call_id,
            status,
        } => {
            format!("agent.tool_completed step={step} id={call_id} status={status:?}")
        }
        AgentEvent::GuardrailTriggered {
            step,
            kind,
            mode,
            threshold,
            observed,
            call_id,
        } => format!(
            "agent.guardrail_triggered step={step} kind={kind:?} mode={mode:?} threshold={threshold} \
             observed={observed} id={call_id}"
        ),
        AgentEvent::ExecutionCompleted {
            step,
            message,
            dropped_events,
        } => format!(
            "agent.execution_completed step={step} message={} dropped={dropped_events}",
            message.id
        ),
        AgentEvent::ExecutionFailed {
            error,
            dropped_events,
        } => format!("agent.execution_failed error={error} dropped={dropped_events}"),
        AgentEvent::ExecutionCancelled { dropped_events } => {
            format!("agent.execution_cancelled dropped={dropped_events}")
        }
        AgentEvent::ExecutionCompactionRequired {
            reason,
            step,
            dropped_events,
            ..
        } => format!(
            "agent.execution_compaction_required reason={reason:?} step={step} \
             dropped={dropped_events}"
        ),
        AgentEvent::ExecutionContinuationRequired {
            reason,
            dropped_events,
            ..
        } => format!(
            "agent.execution_continuation_required reason={reason:?} dropped={dropped_events}"
        ),
    }
}

pub(crate) fn format_outcome(outcome: &ExecutionOutcome) -> String {
    match outcome {
        ExecutionOutcome::Completed { message, .. } => {
            format!("completed message={}", message.id)
        }
        ExecutionOutcome::Failed { error, .. } => format!("failed error={error}"),
        ExecutionOutcome::Cancelled { .. } => "cancelled".to_owned(),
        ExecutionOutcome::CompactionRequired { reason, step, .. } => {
            format!("compaction_required reason={reason:?} step={step}")
        }
        ExecutionOutcome::ContinuationRequired { reason, .. } => {
            format!("continuation_required reason={reason:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        journal::PendingSummary,
        runtime::{
            HarnessRunId, HarnessSessionId, MessageRole, RunSnapshot, RunStatus, RuntimeSnapshot,
            TerminalKind,
        },
    };

    use super::*;

    #[test]
    fn state_projection_contains_structure_but_not_message_bodies_or_credentials() {
        let snapshot = RuntimeSnapshot {
            session_id: HarnessSessionId::from_value("session_test"),
            run: Some(RunSnapshot {
                id: HarnessRunId::from_sequence(3),
                status: RunStatus::Failed,
                created_at_ms: Some(1),
                started_at_ms: Some(2),
                finished_at_ms: Some(14),
                elapsed: Some(Duration::from_millis(12)),
                event_count: 4,
                terminal: Some(TerminalKind::Failed),
                dropped_events: 2,
            }),
            completed_messages: 2,
            roles: vec![MessageRole::User, MessageRole::Assistant],
            effective_roles: vec![MessageRole::ContextSummary, MessageRole::User],
            pending: vec![PendingSummary {
                receipt: "run_3_exchange_1".to_owned(),
                assistant_message_id: "assistant_secret_body_is_elsewhere".to_owned(),
            }],
            checkpoint_count: 1,
            automatic_compactions: 1,
            max_automatic_compactions: 2,
            user_compaction_queued: false,
            last_compaction: None,
        };
        let output = format_state(&snapshot);
        assert!(output.contains("session: session_test"));
        assert!(output.contains("status=failed"));
        assert!(output.contains("user -> assistant"));
        assert!(output.contains("run_3_exchange_1"));
        assert!(output.contains("checkpoints=1"));
        assert!(output.contains("automatic=1/2"));
        assert!(!output.contains("DEEPSEEK_API_KEY"));
        assert!(!output.contains("sk-secret"));
        assert!(!output.contains("actual user message body"));
    }

    #[test]
    fn list_projection_is_driven_by_the_version_registry() {
        let output = list_text();
        assert!(output.contains("v0.1"));
        assert!(output.contains("offline: none"));
        assert!(output.contains("v0.2"));
        assert!(output.contains("offline: verify v0.2 (6 scenarios)"));
        assert!(output.contains("v0.3"));
        assert!(output.contains("offline: verify v0.3 (14 scenarios)"));
        assert!(output.contains("manual: chat"));
    }
}
