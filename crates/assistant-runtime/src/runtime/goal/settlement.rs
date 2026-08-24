//! Goal-bound Run 的预算结算、状态转换与后继事实构造。

use std::collections::BTreeSet;

use agent_types::{ConversationMessage, ConversationSnapshot};
use assistant_protocol::{RunId, RunStatus, SessionId};

use crate::{
    GoalInputBinding, InputOrigin, NewStoredInput, RuntimeError, RuntimeResult,
    StoredGoalSettlementEffect,
    goal::{GoalAgentStatus, GoalControl, GoalPauseReason, GoalState, create_continuation_message},
    run::allocate_run_id,
    session::SessionState,
};

/// 当前 Run 已形成可靠终态前，冻结同一事务需要应用的 Goal effect。
pub(crate) fn prepare_goal_run_settlement(
    state: &SessionState,
    session_id: &SessionId,
    run_id: &RunId,
    status: RunStatus,
    conversation: &ConversationSnapshot,
    new_messages: &[ConversationMessage],
    finished_at_ms: i64,
) -> RuntimeResult<Option<StoredGoalSettlementEffect>> {
    if matches!(status, RunStatus::Cancelled | RunStatus::Interrupted) {
        return Ok(None);
    }
    let run = state
        .runs
        .get(run_id)
        .ok_or(RuntimeError::InternalStateUnavailable {
            component: "Goal settlement Run",
        })?;
    let input = state
        .inputs
        .get(run.input_id())
        .ok_or(RuntimeError::InternalStateUnavailable {
            component: "Goal settlement Input",
        })?;
    let Some(binding) = input.stored.goal_binding.as_ref() else {
        return Ok(None);
    };
    let Some(current) = state.goal.as_ref() else {
        return Ok(None);
    };
    if !matches!(current.state, GoalState::Running)
        || current.id != binding.goal_id
        || current.generation != binding.generation
    {
        return Ok(None);
    }
    let signal = state
        .active_run
        .as_ref()
        .filter(|active| active.run_id == *run_id)
        .and_then(|active| active.goal_signal_latch.as_ref())
        .and_then(|latch| latch.signal())
        .filter(|signal| {
            signal.binding.goal_id == current.id
                && signal.binding.generation == current.generation
                && signal.binding.run_id == *run_id
        });
    let mut goal = current.clone();
    apply_usage(&mut goal, run, conversation, new_messages)?;
    goal.updated_at_ms = finished_at_ms;
    if status == RunStatus::Failed {
        goal.consecutive_failures = goal.consecutive_failures.checked_add(1).ok_or(
            RuntimeError::InternalStateUnavailable {
                component: "Goal consecutive failure budget",
            },
        )?;
    } else {
        goal.consecutive_failures = 0;
    }

    if status == RunStatus::Completed
        && let Some(signal) = signal
    {
        return match signal.status {
            GoalAgentStatus::Complete => transition(
                current,
                goal,
                GoalState::Completed,
                state,
                session_id,
                finished_at_ms,
            ),
            GoalAgentStatus::Blocked => transition(
                current,
                goal,
                GoalState::Paused(GoalPauseReason::Blocked {
                    summary: signal.summary,
                }),
                state,
                session_id,
                finished_at_ms,
            ),
        };
    }
    if goal.budget.used_runs >= goal.budget.max_runs {
        return transition(
            current,
            goal,
            GoalState::Paused(GoalPauseReason::RunLimitReached),
            state,
            session_id,
            finished_at_ms,
        );
    }
    if goal.budget.used_total_tokens >= goal.budget.max_total_tokens {
        return transition(
            current,
            goal,
            GoalState::Paused(GoalPauseReason::TokenLimitReached),
            state,
            session_id,
            finished_at_ms,
        );
    }
    if goal.consecutive_failures >= goal.budget.max_consecutive_failures {
        return transition(
            current,
            goal,
            GoalState::Paused(GoalPauseReason::ConsecutiveFailures),
            state,
            session_id,
            finished_at_ms,
        );
    }

    goal.turn = goal
        .turn
        .checked_add(1)
        .ok_or(RuntimeError::InternalStateUnavailable {
            component: "Goal turn",
        })?;
    let message = create_continuation_message(&goal, run_status_label(status))?;
    let input_id = allocate_input_id(state)?;
    let next_run_id = allocate_run_id(state)?;
    let next_input = NewStoredInput {
        input_id,
        run_id: next_run_id,
        session_id: session_id.clone(),
        idempotency_key: None,
        agent_variant: run.variant(),
        origin: InputOrigin::Runtime,
        goal_binding: Some(GoalInputBinding {
            goal_id: goal.id.clone(),
            generation: goal.generation,
            turn: goal.turn,
        }),
        approval_mode: run.approval_mode(),
        message,
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: finished_at_ms,
    };
    Ok(Some(StoredGoalSettlementEffect::Continue {
        expected_goal_id: current.id.clone(),
        expected_generation: current.generation,
        goal: goal.to_stored(session_id.clone()),
        next_input: Box::new(next_input),
    }))
}

fn apply_usage(
    goal: &mut GoalControl,
    run: &crate::run::RunRecord,
    conversation: &ConversationSnapshot,
    new_messages: &[ConversationMessage],
) -> RuntimeResult<()> {
    goal.budget.used_runs =
        goal.budget
            .used_runs
            .checked_add(1)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "Goal Run budget",
            })?;
    let ids = run
        .message_ids()
        .iter()
        .cloned()
        .chain(new_messages.iter().map(message_id).cloned())
        .collect::<BTreeSet<_>>();
    let assistants = conversation
        .messages
        .iter()
        .chain(new_messages.iter())
        .filter_map(|message| match message {
            ConversationMessage::Assistant(message) if ids.contains(&message.id) => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    let usage_complete =
        !assistants.is_empty() && assistants.iter().all(|message| message.usage.is_some());
    let reported = assistants
        .iter()
        .filter_map(|message| message.usage.as_ref())
        .try_fold(0_u64, |total, usage| total.checked_add(usage.total_tokens))
        .ok_or(RuntimeError::InternalStateUnavailable {
            component: "Goal token budget",
        })?;
    goal.budget.used_total_tokens = goal.budget.used_total_tokens.checked_add(reported).ok_or(
        RuntimeError::InternalStateUnavailable {
            component: "Goal token budget",
        },
    )?;
    goal.budget.usage_complete &= usage_complete;
    Ok(())
}

fn transition(
    current: &GoalControl,
    mut goal: GoalControl,
    state: GoalState,
    session: &SessionState,
    session_id: &SessionId,
    finished_at_ms: i64,
) -> RuntimeResult<Option<StoredGoalSettlementEffect>> {
    goal.generation =
        goal.generation
            .checked_add(1)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "Goal generation",
            })?;
    goal.completed_at_ms = matches!(state, GoalState::Completed).then_some(finished_at_ms);
    goal.state = state;
    Ok(Some(StoredGoalSettlementEffect::Transition {
        expected_goal_id: current.id.clone(),
        expected_generation: current.generation,
        goal: goal.to_stored(session_id.clone()),
        resume_required: !session.user_inputs.is_empty(),
    }))
}

fn allocate_input_id(state: &SessionState) -> RuntimeResult<assistant_protocol::InputId> {
    for _ in 0..crate::id::GENERATION_ATTEMPTS {
        let value =
            crate::id::generate("i").map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "Goal continuation input id random source",
            })?;
        let id = assistant_protocol::InputId::new(value).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "Goal continuation input id",
            }
        })?;
        if !state.inputs.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(RuntimeError::InternalStateUnavailable {
        component: "Goal continuation input id collision",
    })
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Completed => "completed_without_goal_signal",
        RunStatus::Failed => "failed_below_retry_limit",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Interrupted => "interrupted",
        RunStatus::CompactionRequired => "compaction_required",
        RunStatus::Accepted | RunStatus::Running | RunStatus::Cancelling => "non_terminal",
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
