//! Core completion 到规范正文、Store 终态和内存 Run 投影的一致结算。

use std::time::{SystemTime, UNIX_EPOCH};

use agent_core::{ExecutionError, ExecutionOutcome};
use agent_model::ModelError;
use agent_types::{AssistantMessage, AssistantPart, ConversationMessage};
use assistant_protocol::{RunId, RunSnapshot, RunStatus, RuntimeErrorCode, RuntimeErrorInfo};

use crate::{
    RuntimeError, RuntimeResult, RuntimeStore, StoredRunSettlement,
    goal::{GoalControl, GoalState},
    id,
    journal::InMemoryJournal,
    session::{InputRecord, SessionController},
};

use super::{ModelFailureDiagnostics, RunModelDiagnostics, is_active_run};

pub(super) struct RunSettlement {
    pub(super) status: RunStatus,
    pub(super) reasoning: Option<String>,
    pub(super) text: Option<String>,
    pub(super) error: Option<RuntimeErrorInfo>,
}

pub(crate) struct RunSettlementResult {
    pub(crate) run: RunSnapshot,
    pub(crate) continuation: Option<RunSnapshot>,
    pub(crate) goal: Option<assistant_protocol::GoalSnapshot>,
    pub(crate) committed_step: Option<u32>,
}

/// 先完成正文与 Run 的 Store 原子结算，再替换内存投影。
pub(crate) async fn settle_run(
    session: &SessionController,
    run_id: &RunId,
    outcome: Option<ExecutionOutcome>,
    store: &dyn RuntimeStore,
    model_diagnostics: Option<&RunModelDiagnostics>,
) -> RuntimeResult<RunSettlementResult> {
    settle_run_inner(session, run_id, outcome, store, model_diagnostics, None).await
}

/// Runtime 编排（例如自动压缩）失败时，以已经脱敏的稳定错误结算同一 Run。
pub(crate) async fn settle_run_with_error(
    session: &SessionController,
    run_id: &RunId,
    error: RuntimeErrorInfo,
    store: &dyn RuntimeStore,
    model_diagnostics: Option<&RunModelDiagnostics>,
) -> RuntimeResult<RunSettlementResult> {
    settle_run_inner(session, run_id, None, store, model_diagnostics, Some(error)).await
}

async fn settle_run_inner(
    session: &SessionController,
    run_id: &RunId,
    outcome: Option<ExecutionOutcome>,
    store: &dyn RuntimeStore,
    model_diagnostics: Option<&RunModelDiagnostics>,
    forced_error: Option<RuntimeErrorInfo>,
) -> RuntimeResult<RunSettlementResult> {
    let finished_at_ms = system_time_ms()?;
    let final_step = outcome.as_ref().and_then(|outcome| match outcome {
        ExecutionOutcome::Completed { step, .. } => Some(*step),
        _ => None,
    });
    let _mutation = session.mutation().await;
    let (candidate, messages, settlement, cancel_requested, goal_effect) = {
        let mut state = session.lock_state()?;
        if !is_active_run(&state, run_id) {
            state.is_faulted = true;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "run settlement ownership",
            });
        }
        let Some(record) = state.runs.get(run_id) else {
            state.is_faulted = true;
            state.active_run = None;
            return Err(RuntimeError::InternalStateUnavailable {
                component: "run settlement record",
            });
        };
        let cancel_requested = record.snapshot().cancel_requested;
        let Some(journal) = state.journal.as_ref() else {
            state.is_faulted = true;
            state.is_queue_driver_running = false;
            state.active_run = None;
            return Err(RuntimeError::StorageUnavailable {
                operation: "settle run",
                source: None,
            });
        };
        let (journal_snapshot, has_pending) = (journal.snapshot(), journal.has_pending());
        let mut candidate =
            InMemoryJournal::from_snapshot(journal_snapshot.clone()).map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "run settlement conversation",
                }
            })?;
        let mut settlement = if let Some(error) = forced_error {
            RunSettlement {
                status: RunStatus::Failed,
                reasoning: None,
                text: None,
                error: Some(error),
            }
        } else {
            match outcome {
                Some(ExecutionOutcome::Completed { message, .. }) => {
                    match candidate
                        .append_completed(ConversationMessage::Assistant(message.clone()))
                    {
                        Ok(()) => completed_settlement(&message),
                        Err(_) => {
                            state.is_faulted = true;
                            internal_failure("final assistant message could not be committed")
                        }
                    }
                }
                Some(ExecutionOutcome::Failed { error, .. }) => {
                    failed_settlement(&error, model_diagnostics)
                }
                Some(ExecutionOutcome::Cancelled { .. }) => {
                    RunSettlement::terminal(RunStatus::Cancelled)
                }
                Some(ExecutionOutcome::CompactionRequired { .. }) => {
                    RunSettlement::terminal(RunStatus::CompactionRequired)
                }
                Some(ExecutionOutcome::ContinuationRequired { .. }) => {
                    internal_failure("agent continuation escaped the run execution loop")
                }
                None => internal_failure("agent completion task terminated unexpectedly"),
            }
        };
        if has_pending {
            state.is_faulted = true;
            settlement = internal_failure("run ended with an incomplete tool exchange");
        }
        let candidate = candidate.snapshot();
        let messages = candidate
            .messages
            .get(state.persisted_message_count..)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "persisted conversation boundary",
            })?
            .to_vec();
        let goal_effect = crate::runtime::goal::prepare_goal_run_settlement(
            &state,
            session.id(),
            run_id,
            settlement.status,
            &journal_snapshot,
            &messages,
            finished_at_ms,
        )?;
        (
            candidate,
            messages,
            settlement,
            cancel_requested,
            goal_effect,
        )
    };

    let operation_id =
        id::generate("append").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "storage operation id",
        })?;
    let stored_result = match store
        .settle_run(StoredRunSettlement {
            operation_id,
            run_id: run_id.clone(),
            session_id: session.id().clone(),
            status: settlement.status,
            cancel_requested,
            error: settlement.error.clone(),
            messages: messages.clone(),
            message_step: final_step,
            goal_effect,
            finished_at_ms,
        })
        .await
    {
        Ok(result) => result,
        Err(source) => {
            let mut state = session.lock_state()?;
            state.is_faulted = true;
            state.active_run = None;
            return Err(RuntimeError::from_store("settle run", source));
        }
    };

    let mut state = session.lock_state()?;
    let message_ids = messages.iter().map(message_id).cloned().collect::<Vec<_>>();
    state
        .journal
        .as_mut()
        .ok_or(RuntimeError::StorageUnavailable {
            operation: "settle run",
            source: None,
        })?
        .replace_completed(candidate)
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "settled conversation projection",
        })?;
    state.persisted_message_count = state
        .journal
        .as_ref()
        .expect("journal exists after replacement")
        .message_count();
    state.message_count = u64::try_from(state.persisted_message_count).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "conversation message count",
        }
    })?;
    let record = state
        .runs
        .get_mut(run_id)
        .expect("run existence checked before persistence");
    if let Some(step) = final_step {
        record.extend_message_ids_at_step(message_ids, step);
    } else {
        record.extend_message_ids(message_ids);
    }
    record.settle(settlement, finished_at_ms);
    let snapshot = record.snapshot();
    let continuation = stored_result.continuation.map(|accepted| {
        let record = super::RunRecord::accepted(&accepted.run, Vec::new());
        let snapshot = record.snapshot();
        state.goal_inputs.push_back(accepted.input.input_id.clone());
        state.runs.insert(accepted.run.run_id.clone(), record);
        state.inputs.insert(
            accepted.input.input_id.clone(),
            InputRecord {
                stored: accepted.input,
                first_run_id: accepted.run.run_id.clone(),
                latest_run_id: accepted.run.run_id,
            },
        );
        snapshot
    });
    let goal = if let Some(goal) = stored_result.goal {
        let control =
            GoalControl::try_from(goal).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "settled Goal projection",
            })?;
        let snapshot = crate::runtime::product::project_goal(&control)?;
        if control.state == GoalState::Completed {
            state.goal = None;
        } else {
            state.goal = Some(control);
        }
        Some(snapshot)
    } else {
        None
    };
    state.resume_required = stored_result.resume_required;
    state.updated_at_ms = finished_at_ms;
    state.active_run = None;
    Ok(RunSettlementResult {
        run: snapshot,
        continuation,
        goal,
        committed_step: final_step,
    })
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

fn message_id(message: &ConversationMessage) -> &agent_types::MessageId {
    match message {
        ConversationMessage::System(message) => &message.id,
        ConversationMessage::ContextSummary(message) => &message.id,
        ConversationMessage::User(message) => &message.id,
        ConversationMessage::Assistant(message) => &message.id,
        ConversationMessage::Tool(message) => &message.id,
    }
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

fn failed_settlement(
    error: &ExecutionError,
    model_diagnostics: Option<&RunModelDiagnostics>,
) -> RunSettlement {
    let (code, message) = match error {
        ExecutionError::Internal => (
            RuntimeErrorCode::Internal,
            "agent execution task terminated unexpectedly".to_owned(),
        ),
        ExecutionError::Model(error) => {
            let diagnostics = model_diagnostics
                .map(|diagnostics| diagnostics.failure(error))
                .unwrap_or_else(|| fallback_model_diagnostics(error));
            (
                RuntimeErrorCode::ModelExecutionFailed,
                model_failure_message(diagnostics),
            )
        }
        ExecutionError::ContextWindow(_) => (
            RuntimeErrorCode::Internal,
            "conversation context is invalid".to_owned(),
        ),
        ExecutionError::Record(_) => (
            RuntimeErrorCode::Internal,
            "conversation could not be recorded".to_owned(),
        ),
        ExecutionError::BudgetExceeded { .. } => (
            RuntimeErrorCode::Internal,
            "execution budget was exceeded".to_owned(),
        ),
        ExecutionError::GuardrailTriggered { .. } => (
            RuntimeErrorCode::Internal,
            "execution guardrail was triggered".to_owned(),
        ),
    };
    RunSettlement {
        status: RunStatus::Failed,
        reasoning: None,
        text: None,
        error: Some(RuntimeErrorInfo::new(code, message)),
    }
}

fn fallback_model_diagnostics(error: &ModelError) -> ModelFailureDiagnostics {
    ModelFailureDiagnostics {
        kind: super::model_diagnostics::model_failure_kind(error),
        attempts: 1,
        retries: 0,
        stream_established: false,
        output_observed: false,
    }
}

fn model_failure_message(diagnostics: ModelFailureDiagnostics) -> String {
    let stage = if diagnostics.stream_established {
        "after stream establishment"
    } else {
        "before stream establishment"
    };
    format!(
        "model execution failed {stage} (kind={}, attempts={}, retries={}, output_observed={})",
        model_failure_kind_value(diagnostics.kind),
        diagnostics.attempts,
        diagnostics.retries,
        diagnostics.output_observed,
    )
}

fn model_failure_kind_value(kind: assistant_protocol::ModelFailureKind) -> &'static str {
    use assistant_protocol::ModelFailureKind;
    match kind {
        ModelFailureKind::Configuration => "configuration",
        ModelFailureKind::Authentication => "authentication",
        ModelFailureKind::Connection => "connection",
        ModelFailureKind::Timeout => "timeout",
        ModelFailureKind::StreamInterrupted => "stream_interrupted",
        ModelFailureKind::ProviderRejected => "provider_rejected",
        ModelFailureKind::RateLimited => "rate_limited",
        ModelFailureKind::ServiceUnavailable => "service_unavailable",
        ModelFailureKind::ContextOverflow => "context_overflow",
        ModelFailureKind::Protocol => "protocol",
        ModelFailureKind::ToolArguments => "tool_arguments",
        ModelFailureKind::Resource => "resource",
        ModelFailureKind::Cancelled => "cancelled",
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

fn system_time_ms() -> RuntimeResult<i64> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "system clock",
        }
    })?;
    i64::try_from(duration.as_millis()).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "system clock range",
    })
}
