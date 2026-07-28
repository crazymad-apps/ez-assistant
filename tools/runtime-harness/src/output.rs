//! Stable console projections for events, runs, and journal snapshots.

use std::fmt::Write;

use agent_core::{AgentEvent, ExecutionOutcome};

use crate::{
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
    output
}

pub(crate) fn format_event(event: &AgentEvent) -> String {
    match event {
        AgentEvent::ExecutionStarted => "agent.execution_started".to_owned(),
        AgentEvent::StepStarted { step } => format!("agent.step_started step={step}"),
        AgentEvent::TextDelta { id, delta } => {
            format!("agent.text_delta id={id} text={delta}")
        }
        AgentEvent::ReasoningDelta { id, delta } => {
            format!("agent.reasoning_delta id={id} text={delta}")
        }
        AgentEvent::ToolProposed { call } => {
            format!("agent.tool_proposed id={} name={}", call.id, call.name)
        }
        AgentEvent::ToolStarted { call_id } => {
            format!("agent.tool_started id={call_id}")
        }
        AgentEvent::ToolOutput {
            call_id,
            channel,
            chunk,
        } => {
            format!("agent.tool_output id={call_id} channel={channel:?} text={chunk}")
        }
        AgentEvent::ToolCompleted { call_id, status } => {
            format!("agent.tool_completed id={call_id} status={status:?}")
        }
        AgentEvent::ExecutionCompleted {
            message,
            dropped_events,
        } => format!(
            "agent.execution_completed message={} dropped={dropped_events}",
            message.id
        ),
        AgentEvent::ExecutionFailed {
            error,
            dropped_events,
        } => format!("agent.execution_failed error={error} dropped={dropped_events}"),
        AgentEvent::ExecutionCancelled { dropped_events } => {
            format!("agent.execution_cancelled dropped={dropped_events}")
        }
    }
}

pub(crate) fn format_outcome(outcome: &ExecutionOutcome) -> String {
    match outcome {
        ExecutionOutcome::Completed(message) => {
            format!("completed message={}", message.id)
        }
        ExecutionOutcome::Failed(error) => format!("failed error={error}"),
        ExecutionOutcome::Cancelled => "cancelled".to_owned(),
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
            pending: vec![PendingSummary {
                receipt: "run_3_exchange_1".to_owned(),
                assistant_message_id: "assistant_secret_body_is_elsewhere".to_owned(),
            }],
        };
        let output = format_state(&snapshot);
        assert!(output.contains("session: session_test"));
        assert!(output.contains("status=failed"));
        assert!(output.contains("user -> assistant"));
        assert!(output.contains("run_3_exchange_1"));
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
        assert!(output.contains("manual: chat"));
    }
}
