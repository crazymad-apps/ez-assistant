//! Goal-bound Run 的预算结算、状态转换与后继事实构造。

use std::collections::BTreeSet;

use agent_types::{ConversationMessage, ConversationSnapshot};
use assistant_protocol::{RunId, RunStatus, SessionId};

use crate::{
    RuntimeError, RuntimeResult, StoredGoalSettlementEffect,
    goal::{GoalAgentStatus, GoalControl, GoalPauseReason, GoalState, create_continuation_message},
    session::SessionState,
};

/// Goal 门禁对当前 AgentExecution 结果作出的领域决策。
pub(crate) enum GoalRunDecision {
    /// 提交 Goal 进度和隐藏推进消息后，在同一 Run 内启动下一次 AgentExecution。
    Continue {
        effect: StoredGoalSettlementEffect,
        message: agent_types::UserMessage,
    },
    /// 当前 Run 可以进入后续门禁或最终结算；可选 effect 是终态 Goal 转换。
    Settle(Option<StoredGoalSettlementEffect>),
}

/// Goal 门禁判断一次 AgentExecution 结果所需的权威事实。
pub(crate) struct GoalRunFacts<'a> {
    pub(crate) state: &'a SessionState,
    pub(crate) session_id: &'a SessionId,
    pub(crate) run_id: &'a RunId,
    pub(crate) status: RunStatus,
    pub(crate) conversation: &'a ConversationSnapshot,
    pub(crate) new_messages: &'a [ConversationMessage],
    /// 本次 AgentExecution 在所属 Run 全局 step 序列中的闭区间。
    pub(crate) execution_steps: (u32, u32),
    pub(crate) finished_at_ms: i64,
}

/// 当前 AgentExecution 结束后，由 Goal 领域决定是否继续同一 Run。
pub(crate) fn prepare_goal_run_decision(facts: GoalRunFacts<'_>) -> RuntimeResult<GoalRunDecision> {
    let GoalRunFacts {
        state,
        session_id,
        run_id,
        status,
        conversation,
        new_messages,
        execution_steps: (execution_start_step, execution_end_step),
        finished_at_ms,
    } = facts;
    if matches!(status, RunStatus::Cancelled | RunStatus::Interrupted) {
        return Ok(GoalRunDecision::Settle(None));
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
        return Ok(GoalRunDecision::Settle(None));
    };
    let Some(current) = state.goal.as_ref() else {
        return Ok(GoalRunDecision::Settle(None));
    };
    if !matches!(current.state, GoalState::Running)
        || current.id != binding.goal_id
        || current.generation != binding.generation
    {
        return Ok(GoalRunDecision::Settle(None));
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
    apply_usage(
        &mut goal,
        run,
        conversation,
        new_messages,
        execution_start_step,
        execution_end_step,
    )?;
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
            )
            .map(GoalRunDecision::Settle),
            GoalAgentStatus::Blocked => transition(
                current,
                goal,
                GoalState::Paused(GoalPauseReason::Blocked {
                    summary: signal.summary,
                }),
                state,
                session_id,
                finished_at_ms,
            )
            .map(GoalRunDecision::Settle),
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
        )
        .map(GoalRunDecision::Settle);
    }
    if goal.budget.used_total_tokens >= goal.budget.max_total_tokens {
        return transition(
            current,
            goal,
            GoalState::Paused(GoalPauseReason::TokenLimitReached),
            state,
            session_id,
            finished_at_ms,
        )
        .map(GoalRunDecision::Settle);
    }
    if goal.consecutive_failures >= goal.budget.max_consecutive_failures {
        return transition(
            current,
            goal,
            GoalState::Paused(GoalPauseReason::ConsecutiveFailures),
            state,
            session_id,
            finished_at_ms,
        )
        .map(GoalRunDecision::Settle);
    }

    goal.turn = goal
        .turn
        .checked_add(1)
        .ok_or(RuntimeError::InternalStateUnavailable {
            component: "Goal turn",
        })?;
    let message = create_continuation_message(&goal, run_status_label(status))?;
    Ok(GoalRunDecision::Continue {
        effect: StoredGoalSettlementEffect::Progress {
            expected_goal_id: current.id.clone(),
            expected_generation: current.generation,
            goal: goal.to_stored(session_id.clone()),
        },
        message,
    })
}

fn apply_usage(
    goal: &mut GoalControl,
    run: &crate::run::RunRecord,
    conversation: &ConversationSnapshot,
    new_messages: &[ConversationMessage],
    execution_start_step: u32,
    execution_end_step: u32,
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
        .filter(|message_id| {
            run.message_step(message_id)
                .is_some_and(|step| (execution_start_step..=execution_end_step).contains(&step))
        })
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
        resume_required: !session.session_inputs.is_empty(),
    }))
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
