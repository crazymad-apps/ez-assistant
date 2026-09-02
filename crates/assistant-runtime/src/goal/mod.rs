//! Session Goal 权威领域状态与持久恢复校验。

mod context;
mod signal;

use std::collections::BTreeSet;

use agent_types::{FileReferencesPart, MessageId, TextPart, UserMessage, UserPart};
use assistant_protocol::{GoalId, SessionId};
use sha2::{Digest, Sha256};

use crate::{
    StoredGoal, StoredGoalBudget, StoredGoalObjectivePart, StoredGoalPauseReason, StoredGoalState,
};

pub(crate) use context::{
    create_continuation_message, inject_resume_context, inject_start_context,
};
pub(crate) use signal::{
    GoalAgentStatus, GoalRunBinding, GoalRunSignalLatch, GoalSignalAuthorizationFacts,
    UPDATE_GOAL_TOOL_NAME, UpdateGoalTool,
};

pub(crate) const DEFAULT_MAX_RUNS: u32 = 20;
pub(crate) const DEFAULT_MAX_TOTAL_TOKENS: u64 = 500_000;
pub(crate) const DEFAULT_MAX_CONSECUTIVE_FAILURES: u32 = 3;
const OBJECTIVE_HASH_PREFIX: &str = "sha256-v1:";
const MAX_BLOCKED_SUMMARY_BYTES: usize = 8 * 1024;

pub(crate) fn allocate_goal_id() -> crate::RuntimeResult<GoalId> {
    let value =
        crate::id::generate("g").map_err(|_| crate::RuntimeError::InternalStateUnavailable {
            component: "goal id random source",
        })?;
    GoalId::new(value).map_err(|_| crate::RuntimeError::InternalStateUnavailable {
        component: "goal id generator",
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalObjectivePart {
    Text(TextPart),
    FileReferences(FileReferencesPart),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalObjectiveSnapshot {
    pub(crate) source_message_id: MessageId,
    pub(crate) payload: Vec<GoalObjectivePart>,
    pub(crate) payload_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalState {
    Running,
    Paused(GoalPauseReason),
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalPauseReason {
    Blocked { summary: String },
    UserStopped,
    RunLimitReached,
    TokenLimitReached,
    ConsecutiveFailures,
    RecoveryRequired,
    Forked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalBudget {
    pub(crate) max_runs: u32,
    pub(crate) max_total_tokens: u64,
    pub(crate) max_consecutive_failures: u32,
    pub(crate) used_runs: u32,
    pub(crate) used_total_tokens: u64,
    pub(crate) usage_complete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoalControl {
    pub(crate) id: GoalId,
    pub(crate) objective: GoalObjectiveSnapshot,
    pub(crate) state: GoalState,
    pub(crate) generation: u64,
    pub(crate) turn: u32,
    pub(crate) budget: GoalBudget,
    pub(crate) consecutive_failures: u32,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) completed_at_ms: Option<i64>,
}

impl GoalControl {
    pub(crate) fn start(id: GoalId, message: &UserMessage, created_at_ms: i64) -> Result<Self, ()> {
        let payload = message
            .parts
            .iter()
            .filter_map(|part| match part {
                UserPart::Text(part) => Some(GoalObjectivePart::Text(part.clone())),
                UserPart::FileReferences(part) => {
                    Some(GoalObjectivePart::FileReferences(part.clone()))
                }
                UserPart::Injected(_) | UserPart::InternalContext(_) | UserPart::QuotedText(_) => {
                    None
                }
            })
            .collect::<Vec<_>>();
        let payload_hash = objective_hash(&payload)?;
        Ok(Self {
            id,
            objective: GoalObjectiveSnapshot {
                source_message_id: message.id.clone(),
                payload,
                payload_hash,
            },
            state: GoalState::Running,
            generation: 1,
            turn: 1,
            budget: GoalBudget {
                max_runs: DEFAULT_MAX_RUNS,
                max_total_tokens: DEFAULT_MAX_TOTAL_TOKENS,
                max_consecutive_failures: DEFAULT_MAX_CONSECUTIVE_FAILURES,
                used_runs: 0,
                used_total_tokens: 0,
                usage_complete: true,
            },
            consecutive_failures: 0,
            created_at_ms,
            updated_at_ms: created_at_ms,
            completed_at_ms: None,
        })
    }

    pub(crate) fn to_stored(&self, session_id: SessionId) -> StoredGoal {
        let (state, pause_reason) = match &self.state {
            GoalState::Running => (StoredGoalState::Running, None),
            GoalState::Paused(reason) => (
                StoredGoalState::Paused,
                Some(StoredGoalPauseReason::from(reason)),
            ),
            GoalState::Completed => (StoredGoalState::Completed, None),
        };
        StoredGoal {
            goal_id: self.id.clone(),
            session_id,
            objective: crate::StoredGoalObjective {
                source_message_id: self.objective.source_message_id.clone(),
                payload: self
                    .objective
                    .payload
                    .iter()
                    .map(|part| match part {
                        GoalObjectivePart::Text(part) => {
                            StoredGoalObjectivePart::Text(part.clone())
                        }
                        GoalObjectivePart::FileReferences(part) => {
                            StoredGoalObjectivePart::FileReferences(part.clone())
                        }
                    })
                    .collect(),
                payload_hash: self.objective.payload_hash.clone(),
            },
            state,
            pause_reason,
            generation: self.generation,
            turn: self.turn,
            budget: StoredGoalBudget {
                max_runs: self.budget.max_runs,
                max_total_tokens: self.budget.max_total_tokens,
                max_consecutive_failures: self.budget.max_consecutive_failures,
                used_runs: self.budget.used_runs,
                used_total_tokens: self.budget.used_total_tokens,
                usage_complete: self.budget.usage_complete,
            },
            consecutive_failures: self.consecutive_failures,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            completed_at_ms: self.completed_at_ms,
        }
    }

    pub(crate) fn resume(&self, resumed_at_ms: i64) -> Result<Self, ()> {
        if !matches!(self.state, GoalState::Paused(_)) || resumed_at_ms < self.updated_at_ms {
            return Err(());
        }
        let mut resumed = self.clone();
        resumed.state = GoalState::Running;
        resumed.generation = resumed.generation.checked_add(1).ok_or(())?;
        resumed.turn = resumed.turn.checked_add(1).ok_or(())?;
        resumed.updated_at_ms = resumed_at_ms;
        resumed.completed_at_ms = None;
        Ok(resumed)
    }

    pub(crate) fn stop(&self, stopped_at_ms: i64) -> Result<Self, ()> {
        if !matches!(self.state, GoalState::Running) || stopped_at_ms < self.updated_at_ms {
            return Err(());
        }
        let mut stopped = self.clone();
        stopped.state = GoalState::Paused(GoalPauseReason::UserStopped);
        stopped.generation = stopped.generation.checked_add(1).ok_or(())?;
        stopped.updated_at_ms = stopped_at_ms;
        stopped.completed_at_ms = None;
        Ok(stopped)
    }

    pub(crate) fn forked(&self, id: GoalId, forked_at_ms: i64) -> Self {
        let mut forked = self.clone();
        forked.id = id;
        forked.state = GoalState::Paused(GoalPauseReason::Forked);
        forked.generation = 1;
        forked.created_at_ms = forked_at_ms;
        forked.updated_at_ms = forked_at_ms;
        forked.completed_at_ms = None;
        forked
    }

    pub(crate) fn paused_for_recovery(&self, changed_at_ms: i64) -> Result<Self, ()> {
        if changed_at_ms < self.updated_at_ms {
            return Err(());
        }
        let mut paused = self.clone();
        paused.state = GoalState::Paused(GoalPauseReason::RecoveryRequired);
        paused.generation = paused.generation.checked_add(1).ok_or(())?;
        paused.updated_at_ms = changed_at_ms;
        paused.completed_at_ms = None;
        Ok(paused)
    }

    pub(crate) fn stored_objective_parts(&self) -> Vec<StoredGoalObjectivePart> {
        self.objective
            .payload
            .iter()
            .map(|part| match part {
                GoalObjectivePart::Text(part) => StoredGoalObjectivePart::Text(part.clone()),
                GoalObjectivePart::FileReferences(part) => {
                    StoredGoalObjectivePart::FileReferences(part.clone())
                }
            })
            .collect()
    }
}

impl From<&GoalPauseReason> for StoredGoalPauseReason {
    fn from(value: &GoalPauseReason) -> Self {
        match value {
            GoalPauseReason::Blocked { summary } => Self::Blocked {
                summary: summary.clone(),
            },
            GoalPauseReason::UserStopped => Self::UserStopped,
            GoalPauseReason::RunLimitReached => Self::RunLimitReached,
            GoalPauseReason::TokenLimitReached => Self::TokenLimitReached,
            GoalPauseReason::ConsecutiveFailures => Self::ConsecutiveFailures,
            GoalPauseReason::RecoveryRequired => Self::RecoveryRequired,
            GoalPauseReason::Forked => Self::Forked,
        }
    }
}

impl TryFrom<StoredGoal> for GoalControl {
    type Error = ();

    fn try_from(value: StoredGoal) -> Result<Self, Self::Error> {
        let payload = value
            .objective
            .payload
            .into_iter()
            .map(GoalObjectivePart::from)
            .collect::<Vec<_>>();
        validate_objective_payload(&payload, &value.objective.payload_hash)?;
        let state = match (value.state, value.pause_reason) {
            (StoredGoalState::Running, None) => GoalState::Running,
            (StoredGoalState::Paused, Some(reason)) => {
                GoalState::Paused(GoalPauseReason::try_from(reason)?)
            }
            (StoredGoalState::Completed, None) => GoalState::Completed,
            _ => return Err(()),
        };
        let budget = GoalBudget::try_from(value.budget)?;
        if value.generation == 0
            || value.turn == 0
            || value.consecutive_failures > budget.max_consecutive_failures
            || value.updated_at_ms < value.created_at_ms
            || value.completed_at_ms.is_some_and(|completed| {
                completed < value.created_at_ms || completed > value.updated_at_ms
            })
            || matches!(state, GoalState::Completed) != value.completed_at_ms.is_some()
        {
            return Err(());
        }
        Ok(Self {
            id: value.goal_id,
            objective: GoalObjectiveSnapshot {
                source_message_id: value.objective.source_message_id,
                payload,
                payload_hash: value.objective.payload_hash,
            },
            state,
            generation: value.generation,
            turn: value.turn,
            budget,
            consecutive_failures: value.consecutive_failures,
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
            completed_at_ms: value.completed_at_ms,
        })
    }
}

impl TryFrom<StoredGoalBudget> for GoalBudget {
    type Error = ();

    fn try_from(value: StoredGoalBudget) -> Result<Self, Self::Error> {
        if value.max_runs == 0
            || value.max_total_tokens == 0
            || value.max_consecutive_failures == 0
            || value.max_runs != DEFAULT_MAX_RUNS
            || value.max_total_tokens != DEFAULT_MAX_TOTAL_TOKENS
            || value.max_consecutive_failures != DEFAULT_MAX_CONSECUTIVE_FAILURES
            || value.used_runs > value.max_runs
        {
            return Err(());
        }
        Ok(Self {
            max_runs: value.max_runs,
            max_total_tokens: value.max_total_tokens,
            max_consecutive_failures: value.max_consecutive_failures,
            used_runs: value.used_runs,
            used_total_tokens: value.used_total_tokens,
            usage_complete: value.usage_complete,
        })
    }
}

impl TryFrom<StoredGoalPauseReason> for GoalPauseReason {
    type Error = ();

    fn try_from(value: StoredGoalPauseReason) -> Result<Self, Self::Error> {
        Ok(match value {
            StoredGoalPauseReason::Blocked { summary } => {
                if summary.trim().is_empty() || summary.len() > MAX_BLOCKED_SUMMARY_BYTES {
                    return Err(());
                }
                Self::Blocked { summary }
            }
            StoredGoalPauseReason::UserStopped => Self::UserStopped,
            StoredGoalPauseReason::RunLimitReached => Self::RunLimitReached,
            StoredGoalPauseReason::TokenLimitReached => Self::TokenLimitReached,
            StoredGoalPauseReason::ConsecutiveFailures => Self::ConsecutiveFailures,
            StoredGoalPauseReason::RecoveryRequired => Self::RecoveryRequired,
            StoredGoalPauseReason::Forked => Self::Forked,
        })
    }
}

impl From<StoredGoalObjectivePart> for GoalObjectivePart {
    fn from(value: StoredGoalObjectivePart) -> Self {
        match value {
            StoredGoalObjectivePart::Text(part) => Self::Text(part),
            StoredGoalObjectivePart::FileReferences(part) => Self::FileReferences(part),
        }
    }
}

fn validate_objective_payload(
    payload: &[GoalObjectivePart],
    expected_hash: &str,
) -> Result<(), ()> {
    let actual = objective_hash(payload)?;
    (actual == expected_hash).then_some(()).ok_or(())
}

fn objective_hash(payload: &[GoalObjectivePart]) -> Result<String, ()> {
    if payload.is_empty() {
        return Err(());
    }
    let mut part_ids = BTreeSet::new();
    let stored = payload
        .iter()
        .map(|part| match part {
            GoalObjectivePart::Text(part) => {
                if part.text.trim().is_empty() || !part_ids.insert(part.id.clone()) {
                    return Err(());
                }
                Ok(StoredGoalObjectivePart::Text(part.clone()))
            }
            GoalObjectivePart::FileReferences(part) => {
                if part.files.is_empty() || !part_ids.insert(part.id.clone()) {
                    return Err(());
                }
                Ok(StoredGoalObjectivePart::FileReferences(part.clone()))
            }
        })
        .collect::<Result<Vec<_>, ()>>()?;
    let encoded = serde_json::to_vec(&stored).map_err(|_| ())?;
    let digest = Sha256::digest(encoded);
    Ok(format!("{OBJECTIVE_HASH_PREFIX}{digest:x}"))
}

#[cfg(test)]
mod tests {
    use agent_types::PartId;
    use assistant_protocol::SessionId;

    use super::*;
    use crate::{StoredGoalObjective, StoredGoalState};

    fn stored_goal(state: StoredGoalState, reason: Option<StoredGoalPauseReason>) -> StoredGoal {
        let part = StoredGoalObjectivePart::Text(TextPart {
            id: PartId::new("objective-text").expect("part id"),
            text: "ship the release".to_owned(),
        });
        let encoded = serde_json::to_vec(&vec![part.clone()]).expect("encode payload");
        let hash = format!("{OBJECTIVE_HASH_PREFIX}{:x}", Sha256::digest(encoded));
        StoredGoal {
            goal_id: GoalId::new("goal-1").expect("goal id"),
            session_id: SessionId::new("session-1").expect("session id"),
            objective: StoredGoalObjective {
                source_message_id: MessageId::new("message-1").expect("message id"),
                payload: vec![part],
                payload_hash: hash,
            },
            state,
            pause_reason: reason,
            generation: 1,
            turn: 1,
            budget: StoredGoalBudget {
                max_runs: DEFAULT_MAX_RUNS,
                max_total_tokens: DEFAULT_MAX_TOTAL_TOKENS,
                max_consecutive_failures: DEFAULT_MAX_CONSECUTIVE_FAILURES,
                used_runs: 0,
                used_total_tokens: 0,
                usage_complete: true,
            },
            consecutive_failures: 0,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        }
    }

    #[test]
    fn validates_objective_hash_state_and_budget() {
        let goal =
            GoalControl::try_from(stored_goal(StoredGoalState::Running, None)).expect("valid goal");
        assert_eq!(goal.generation, 1);
        assert_eq!(goal.budget.max_runs, DEFAULT_MAX_RUNS);

        let mut invalid = stored_goal(StoredGoalState::Running, None);
        invalid.objective.payload_hash = "sha256-v1:wrong".to_owned();
        assert!(GoalControl::try_from(invalid).is_err());

        assert!(
            GoalControl::try_from(stored_goal(
                StoredGoalState::Paused,
                Some(StoredGoalPauseReason::Blocked {
                    summary: " ".to_owned(),
                }),
            ))
            .is_err()
        );
    }
}
