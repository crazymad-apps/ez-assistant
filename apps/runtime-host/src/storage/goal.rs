//! Session Goal 的恢复暂停与持久快照解析。

use agent_types::MessageId;
use assistant_protocol::{GoalId, InputId, RunId, SessionId};
use assistant_runtime::{
    GoalClear, GoalStop, GoalStopResult, StoredGoal, StoredGoalBudget, StoredGoalObjective,
    StoredGoalObjectivePart, StoredGoalPauseReason, StoredGoalState,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, internal_error, invalid_data,
    invalid_data_with_source, non_negative_u64, positive_u64,
};

impl StorageEngine {
    pub(super) fn pause_running_goals_for_recovery(&mut self) -> StorageResult<()> {
        let reason =
            serde_json::to_string(&StoredGoalPauseReason::RecoveryRequired).map_err(|source| {
                internal_error("goal recovery reason could not be encoded", source)
            })?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("goal recovery transaction could not be started", source)
            })?;
        transaction
            .execute(
                "UPDATE session_goals
                 SET state = 'paused', pause_reason_json = ?1, generation = generation + 1
                 WHERE state = 'running' AND generation < 9223372036854775807",
                [reason],
            )
            .map_err(|source| database_write_error("running goals could not be paused", source))?;
        let exhausted = transaction
            .query_row(
                "SELECT 1 FROM session_goals WHERE state = 'running' LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .map_err(|source| internal_error("running goals could not be checked", source))?;
        if exhausted.is_some() {
            return Err(invalid_data("stored goal generation is exhausted"));
        }
        transaction
            .execute(
                "DELETE FROM inputs
                 WHERE state = 'queued' AND origin = 'runtime' AND goal_id IS NOT NULL
                   AND EXISTS (
                       SELECT 1 FROM session_goals
                       WHERE session_goals.session_id = inputs.session_id
                         AND session_goals.goal_id = inputs.goal_id
                         AND inputs.goal_generation < session_goals.generation
                   )",
                [],
            )
            .map_err(|source| {
                database_write_error("stale Goal continuations could not be removed", source)
            })?;
        transaction.commit().map_err(|source| {
            database_write_error("goal recovery transaction could not be committed", source)
        })?;
        Ok(())
    }

    pub(super) fn load_all_goals(&self) -> StorageResult<Vec<StoredGoal>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT goal_id, session_id, objective_message_id, objective_payload_json,
                        objective_hash, state, pause_reason_json, generation, turn, max_runs,
                        max_total_tokens, max_consecutive_failures, used_runs, used_total_tokens,
                        usage_complete, consecutive_failures, created_at_ms, updated_at_ms,
                        completed_at_ms
                 FROM session_goals ORDER BY session_id",
            )
            .map_err(|source| internal_error("goals could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, Option<i64>>(18)?,
                ))
            })
            .map_err(|source| internal_error("goals could not be read", source))?;
        rows.map(|row| {
            let row = row.map_err(|source| internal_error("goal row could not be read", source))?;
            parse_goal(row)
        })
        .collect()
    }

    pub(super) fn stop_goal(&mut self, stop: GoalStop) -> StorageResult<GoalStopResult> {
        let goal = &stop.stopped_goal;
        if goal.goal_id != stop.goal_id
            || goal.session_id != stop.session_id
            || goal.state != StoredGoalState::Paused
            || goal.pause_reason != Some(StoredGoalPauseReason::UserStopped)
            || goal.generation
                != stop
                    .expected_generation
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("Goal generation is exhausted"))?
            || goal.completed_at_ms.is_some()
        {
            return Err(invalid_data("stopped Goal projection is invalid"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("Goal stop could not begin", source))?;
        let mut queued = transaction
            .prepare(
                "SELECT input_id FROM inputs
                 WHERE session_id = ?1 AND goal_id = ?2 AND goal_generation = ?3
                   AND origin = 'runtime' AND state = 'queued'",
            )
            .map_err(|source| internal_error("Goal continuations could not be queried", source))?;
        let removed_input_ids = queued
            .query_map(
                params![
                    stop.session_id.as_str(),
                    stop.goal_id.as_str(),
                    i64::try_from(stop.expected_generation).map_err(|source| internal_error(
                        "Goal generation exceeds storage range",
                        source
                    ))?,
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| internal_error("Goal continuations could not be read", source))?
            .map(|row| {
                InputId::new(row.map_err(|source| {
                    internal_error("Goal continuation row could not be read", source)
                })?)
                .map_err(|source| invalid_data_with_source("stored input id is invalid", source))
            })
            .collect::<StorageResult<Vec<_>>>()?;
        drop(queued);
        if removed_input_ids.len() > 1 {
            return Err(invalid_data("Goal has multiple queued continuations"));
        }
        let cancelling_run_id = transaction
            .query_row(
                "SELECT runs.run_id FROM runs
                 JOIN inputs ON inputs.input_id = runs.input_id
                 WHERE inputs.session_id = ?1 AND inputs.goal_id = ?2
                   AND inputs.goal_generation = ?3 AND inputs.state = 'committed'
                   AND runs.status NOT IN ('completed','failed','cancelled','interrupted')
                 LIMIT 2",
                params![
                    stop.session_id.as_str(),
                    stop.goal_id.as_str(),
                    i64::try_from(stop.expected_generation).map_err(|source| internal_error(
                        "Goal generation exceeds storage range",
                        source
                    ))?,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("active Goal Run could not be queried", source))?
            .map(|value| {
                RunId::new(value)
                    .map_err(|source| invalid_data_with_source("stored run id is invalid", source))
            })
            .transpose()?;
        let pause_reason = serde_json::to_string(&StoredGoalPauseReason::UserStopped)
            .map_err(|source| internal_error("Goal stop reason could not be encoded", source))?;
        let changed = transaction
            .execute(
                "UPDATE session_goals
                 SET state = 'paused', pause_reason_json = ?1, generation = ?2,
                     updated_at_ms = ?3
                 WHERE goal_id = ?4 AND session_id = ?5 AND state = 'running'
                   AND generation = ?6 AND turn = ?7
                   AND objective_message_id = ?8 AND objective_hash = ?9
                   AND max_runs = ?10 AND max_total_tokens = ?11
                   AND max_consecutive_failures = ?12 AND used_runs = ?13
                   AND used_total_tokens = ?14 AND usage_complete = ?15
                   AND consecutive_failures = ?16 AND created_at_ms = ?17
                   AND completed_at_ms IS NULL AND updated_at_ms <= ?3",
                params![
                    pause_reason,
                    i64::try_from(goal.generation).map_err(|source| internal_error(
                        "Goal generation exceeds storage range",
                        source
                    ))?,
                    goal.updated_at_ms,
                    goal.goal_id.as_str(),
                    goal.session_id.as_str(),
                    i64::try_from(stop.expected_generation).map_err(|source| internal_error(
                        "expected Goal generation exceeds storage range",
                        source
                    ))?,
                    i64::from(goal.turn),
                    goal.objective.source_message_id.as_str(),
                    goal.objective.payload_hash,
                    i64::from(goal.budget.max_runs),
                    i64::try_from(goal.budget.max_total_tokens).map_err(
                        |source| internal_error("Goal token limit exceeds storage range", source)
                    )?,
                    i64::from(goal.budget.max_consecutive_failures),
                    i64::from(goal.budget.used_runs),
                    i64::try_from(goal.budget.used_total_tokens).map_err(|source| {
                        internal_error("Goal token usage exceeds storage range", source)
                    })?,
                    i64::from(goal.budget.usage_complete),
                    i64::from(goal.consecutive_failures),
                    goal.created_at_ms,
                ],
            )
            .map_err(|source| database_write_error("Goal could not be stopped", source))?;
        if changed != 1 {
            return Err(conflict("Goal stop generation is stale"));
        }
        if let Some(run_id) = cancelling_run_id.as_ref() {
            let changed = transaction
                .execute(
                    "UPDATE runs SET status = 'cancelling', cancel_requested = 1
                     WHERE run_id = ?1 AND status NOT IN ('completed','failed','cancelled','interrupted')",
                    [run_id.as_str()],
                )
                .map_err(|source| database_write_error("Goal Run cancel intent could not be recorded", source))?;
            if changed != 1 {
                return Err(conflict("active Goal Run changed during stop"));
            }
        }
        for input_id in &removed_input_ids {
            let changed = transaction
                .execute(
                    "DELETE FROM inputs WHERE input_id = ?1 AND state = 'queued'",
                    [input_id.as_str()],
                )
                .map_err(|source| {
                    database_write_error("Goal continuation could not be removed", source)
                })?;
            if changed != 1 {
                return Err(conflict("Goal continuation changed during stop"));
            }
        }
        transaction
            .commit()
            .map_err(|source| database_write_error("Goal stop could not be committed", source))?;
        Ok(GoalStopResult {
            goal: stop.stopped_goal,
            removed_input_ids,
            cancelling_run_id,
        })
    }

    pub(super) fn clear_goal(&mut self, clear: GoalClear) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| internal_error("Goal clear could not begin", source))?;
        let active = transaction
            .query_row(
                "SELECT 1 FROM runs JOIN inputs ON inputs.input_id = runs.input_id
                 WHERE inputs.session_id = ?1 AND inputs.goal_id = ?2
                   AND runs.status NOT IN ('completed','failed','cancelled','interrupted') LIMIT 1",
                params![clear.session_id.as_str(), clear.goal_id.as_str()],
                |_| Ok(()),
            )
            .optional()
            .map_err(|source| internal_error("active Goal Run could not be checked", source))?;
        if active.is_some() {
            return Err(conflict("Goal still has an active Run"));
        }
        let changed = transaction
            .execute(
                "DELETE FROM session_goals
                 WHERE session_id = ?1 AND goal_id = ?2 AND generation = ?3
                   AND state IN ('paused','completed')",
                params![
                    clear.session_id.as_str(),
                    clear.goal_id.as_str(),
                    i64::try_from(clear.expected_generation).map_err(|source| internal_error(
                        "Goal generation exceeds storage range",
                        source
                    ))?,
                ],
            )
            .map_err(|source| database_write_error("Goal could not be cleared", source))?;
        if changed != 1 {
            return Err(conflict("Goal clear generation is stale"));
        }
        transaction
            .commit()
            .map_err(|source| database_write_error("Goal clear could not be committed", source))?;
        Ok(())
    }
}

pub(super) fn apply_goal_settlement(
    transaction: &Transaction<'_>,
    expected_goal_id: &GoalId,
    expected_generation: u64,
    goal: &StoredGoal,
    finished_at_ms: i64,
) -> StorageResult<()> {
    if goal.goal_id != *expected_goal_id
        || goal.generation < expected_generation
        || goal.updated_at_ms != finished_at_ms
    {
        return Err(invalid_data("Goal settlement projection is invalid"));
    }
    let pause_reason_json = goal
        .pause_reason
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|source| internal_error("Goal pause reason could not be encoded", source))?;
    let state = match goal.state {
        StoredGoalState::Running => "running",
        StoredGoalState::Paused => "paused",
        StoredGoalState::Completed => "completed",
    };
    let changed = transaction
        .execute(
            "UPDATE session_goals
             SET state = ?1, pause_reason_json = ?2, generation = ?3, turn = ?4,
                 used_runs = ?5, used_total_tokens = ?6, usage_complete = ?7,
                 consecutive_failures = ?8, updated_at_ms = ?9, completed_at_ms = ?10
             WHERE goal_id = ?11 AND session_id = ?12 AND generation = ?13
               AND state = 'running' AND objective_message_id = ?14 AND objective_hash = ?15
               AND max_runs = ?16 AND max_total_tokens = ?17
               AND max_consecutive_failures = ?18",
            params![
                state,
                pause_reason_json,
                i64::try_from(goal.generation).map_err(|source| internal_error(
                    "Goal generation exceeds storage range",
                    source
                ))?,
                i64::from(goal.turn),
                i64::from(goal.budget.used_runs),
                i64::try_from(goal.budget.used_total_tokens).map_err(|source| internal_error(
                    "Goal token usage exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.usage_complete),
                i64::from(goal.consecutive_failures),
                goal.updated_at_ms,
                goal.completed_at_ms,
                expected_goal_id.as_str(),
                goal.session_id.as_str(),
                i64::try_from(expected_generation).map_err(|source| internal_error(
                    "expected Goal generation exceeds storage range",
                    source
                ))?,
                goal.objective.source_message_id.as_str(),
                goal.objective.payload_hash,
                i64::from(goal.budget.max_runs),
                i64::try_from(goal.budget.max_total_tokens).map_err(|source| internal_error(
                    "Goal token limit exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.max_consecutive_failures),
            ],
        )
        .map_err(|source| database_write_error("Goal settlement could not be applied", source))?;
    if changed != 1 {
        return Err(conflict("Goal settlement generation is stale"));
    }
    if goal.state == StoredGoalState::Completed {
        let deleted = transaction
            .execute(
                "DELETE FROM session_goals
                 WHERE goal_id = ?1 AND session_id = ?2 AND generation = ?3
                   AND state = 'completed'",
                params![
                    goal.goal_id.as_str(),
                    goal.session_id.as_str(),
                    i64::try_from(goal.generation).map_err(|source| internal_error(
                        "Goal generation exceeds storage range",
                        source
                    ))?,
                ],
            )
            .map_err(|source| {
                database_write_error("completed Goal could not be cleared", source)
            })?;
        if deleted != 1 {
            return Err(conflict("completed Goal could not be cleared"));
        }
    }
    Ok(())
}

pub(super) fn apply_goal_resume(
    transaction: &Transaction<'_>,
    goal: &StoredGoal,
) -> StorageResult<()> {
    if goal.state != StoredGoalState::Running
        || goal.pause_reason.is_some()
        || goal.generation < 2
        || goal.turn < 2
        || goal.completed_at_ms.is_some()
    {
        return Err(invalid_data("resumed Goal projection is invalid"));
    }
    let previous_generation = goal.generation - 1;
    let previous_turn = goal.turn - 1;
    let changed = transaction
        .execute(
            "UPDATE session_goals
             SET state = 'running', pause_reason_json = NULL, generation = ?1, turn = ?2,
                 updated_at_ms = ?3
             WHERE goal_id = ?4 AND session_id = ?5 AND state = 'paused'
               AND generation = ?6 AND turn = ?7
               AND objective_message_id = ?8 AND objective_hash = ?9
               AND max_runs = ?10 AND max_total_tokens = ?11
               AND max_consecutive_failures = ?12 AND used_runs = ?13
               AND used_total_tokens = ?14 AND usage_complete = ?15
               AND consecutive_failures = ?16 AND created_at_ms = ?17
               AND completed_at_ms IS NULL AND updated_at_ms <= ?3",
            params![
                i64::try_from(goal.generation).map_err(|source| internal_error(
                    "Goal generation exceeds storage range",
                    source
                ))?,
                i64::from(goal.turn),
                goal.updated_at_ms,
                goal.goal_id.as_str(),
                goal.session_id.as_str(),
                i64::try_from(previous_generation).map_err(|source| internal_error(
                    "previous Goal generation exceeds storage range",
                    source
                ))?,
                i64::from(previous_turn),
                goal.objective.source_message_id.as_str(),
                goal.objective.payload_hash,
                i64::from(goal.budget.max_runs),
                i64::try_from(goal.budget.max_total_tokens).map_err(|source| internal_error(
                    "Goal token limit exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.max_consecutive_failures),
                i64::from(goal.budget.used_runs),
                i64::try_from(goal.budget.used_total_tokens).map_err(|source| {
                    internal_error("Goal token usage exceeds storage range", source)
                })?,
                i64::from(goal.budget.usage_complete),
                i64::from(goal.consecutive_failures),
                goal.created_at_ms,
            ],
        )
        .map_err(|source| database_write_error("Goal resume could not be applied", source))?;
    if changed != 1 {
        return Err(conflict("Goal resume generation is stale"));
    }
    Ok(())
}

pub(super) fn apply_goal_rewrite_pause(
    transaction: &Transaction<'_>,
    effect: &assistant_runtime::RewriteGoalEffect,
    changed_at_ms: i64,
) -> StorageResult<()> {
    let goal = &effect.goal;
    if goal.goal_id != effect.expected_goal_id
        || goal.state != StoredGoalState::Paused
        || goal.pause_reason != Some(StoredGoalPauseReason::RecoveryRequired)
        || goal.generation
            != effect
                .expected_generation
                .checked_add(1)
                .ok_or_else(|| invalid_data("Goal generation is exhausted"))?
        || goal.updated_at_ms != changed_at_ms
        || goal.completed_at_ms.is_some()
    {
        return Err(invalid_data("history rewrite Goal projection is invalid"));
    }
    let reason = serde_json::to_string(&StoredGoalPauseReason::RecoveryRequired)
        .map_err(|source| internal_error("Goal recovery reason could not be encoded", source))?;
    let changed = transaction
        .execute(
            "UPDATE session_goals
             SET state = 'paused', pause_reason_json = ?1, generation = ?2,
                 updated_at_ms = ?3, completed_at_ms = NULL
             WHERE goal_id = ?4 AND session_id = ?5 AND generation = ?6 AND turn = ?7
               AND objective_message_id = ?8 AND objective_hash = ?9
               AND max_runs = ?10 AND max_total_tokens = ?11
               AND max_consecutive_failures = ?12 AND used_runs = ?13
               AND used_total_tokens = ?14 AND usage_complete = ?15
               AND consecutive_failures = ?16 AND created_at_ms = ?17",
            params![
                reason,
                i64::try_from(goal.generation).map_err(|source| internal_error(
                    "Goal generation exceeds storage range",
                    source
                ))?,
                changed_at_ms,
                goal.goal_id.as_str(),
                goal.session_id.as_str(),
                i64::try_from(effect.expected_generation).map_err(|source| internal_error(
                    "expected Goal generation exceeds storage range",
                    source
                ))?,
                i64::from(goal.turn),
                goal.objective.source_message_id.as_str(),
                goal.objective.payload_hash,
                i64::from(goal.budget.max_runs),
                i64::try_from(goal.budget.max_total_tokens).map_err(|source| internal_error(
                    "Goal token limit exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.max_consecutive_failures),
                i64::from(goal.budget.used_runs),
                i64::try_from(goal.budget.used_total_tokens).map_err(|source| {
                    internal_error("Goal token usage exceeds storage range", source)
                })?,
                i64::from(goal.budget.usage_complete),
                i64::from(goal.consecutive_failures),
                goal.created_at_ms,
            ],
        )
        .map_err(|source| {
            database_write_error("history rewrite Goal could not be paused", source)
        })?;
    if changed != 1 {
        return Err(conflict("history rewrite Goal generation is stale"));
    }
    Ok(())
}

pub(super) fn insert_new_goal(
    transaction: &Transaction<'_>,
    goal: &StoredGoal,
) -> StorageResult<()> {
    if goal.state != StoredGoalState::Running
        || goal.pause_reason.is_some()
        || goal.generation == 0
        || goal.turn == 0
        || goal.completed_at_ms.is_some()
    {
        return Err(invalid_data("new goal state is invalid"));
    }
    let payload = serde_json::to_string(&goal.objective.payload)
        .map_err(|source| internal_error("goal objective could not be encoded", source))?;
    transaction
        .execute(
            "INSERT INTO session_goals (
                goal_id, session_id, objective_message_id, objective_payload_json,
                objective_hash, state, pause_reason_json, generation, turn, max_runs,
                max_total_tokens, max_consecutive_failures, used_runs, used_total_tokens,
                usage_complete, consecutive_failures, created_at_ms, updated_at_ms,
                completed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'running', NULL, ?6, ?7, ?8, ?9, ?10,
                       ?11, ?12, ?13, ?14, ?15, ?16, NULL)",
            params![
                goal.goal_id.as_str(),
                goal.session_id.as_str(),
                goal.objective.source_message_id.as_str(),
                payload,
                goal.objective.payload_hash,
                i64::try_from(goal.generation).map_err(|source| internal_error(
                    "goal generation exceeds storage range",
                    source
                ))?,
                i64::from(goal.turn),
                i64::from(goal.budget.max_runs),
                i64::try_from(goal.budget.max_total_tokens).map_err(|source| internal_error(
                    "goal token limit exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.max_consecutive_failures),
                i64::from(goal.budget.used_runs),
                i64::try_from(goal.budget.used_total_tokens).map_err(|source| internal_error(
                    "goal token usage exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.usage_complete),
                i64::from(goal.consecutive_failures),
                goal.created_at_ms,
                goal.updated_at_ms,
            ],
        )
        .map_err(|source| database_write_error("goal could not be created", source))?;
    Ok(())
}

pub(super) fn insert_forked_goal(
    transaction: &Transaction<'_>,
    goal: &StoredGoal,
) -> StorageResult<()> {
    if goal.state != StoredGoalState::Paused
        || goal.pause_reason != Some(StoredGoalPauseReason::Forked)
        || goal.generation != 1
        || goal.turn == 0
        || goal.completed_at_ms.is_some()
        || goal.created_at_ms != goal.updated_at_ms
    {
        return Err(invalid_data("forked Goal state is invalid"));
    }
    let payload = serde_json::to_string(&goal.objective.payload)
        .map_err(|source| internal_error("forked Goal objective could not be encoded", source))?;
    let pause_reason = serde_json::to_string(&StoredGoalPauseReason::Forked)
        .map_err(|source| internal_error("forked Goal reason could not be encoded", source))?;
    transaction
        .execute(
            "INSERT INTO session_goals (
                goal_id, session_id, objective_message_id, objective_payload_json,
                objective_hash, state, pause_reason_json, generation, turn, max_runs,
                max_total_tokens, max_consecutive_failures, used_runs, used_total_tokens,
                usage_complete, consecutive_failures, created_at_ms, updated_at_ms,
                completed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'paused', ?6, 1, ?7, ?8, ?9, ?10,
                       ?11, ?12, ?13, ?14, ?15, ?15, NULL)",
            params![
                goal.goal_id.as_str(),
                goal.session_id.as_str(),
                goal.objective.source_message_id.as_str(),
                payload,
                goal.objective.payload_hash,
                pause_reason,
                i64::from(goal.turn),
                i64::from(goal.budget.max_runs),
                i64::try_from(goal.budget.max_total_tokens).map_err(|source| internal_error(
                    "Goal token limit exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.max_consecutive_failures),
                i64::from(goal.budget.used_runs),
                i64::try_from(goal.budget.used_total_tokens).map_err(|source| internal_error(
                    "Goal token usage exceeds storage range",
                    source
                ))?,
                i64::from(goal.budget.usage_complete),
                i64::from(goal.consecutive_failures),
                goal.created_at_ms,
            ],
        )
        .map_err(|source| database_write_error("forked Goal could not be created", source))?;
    Ok(())
}

#[allow(clippy::type_complexity)]
fn parse_goal(
    row: (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
    ),
) -> StorageResult<StoredGoal> {
    let (
        goal_id,
        session_id,
        objective_message_id,
        objective_payload_json,
        objective_hash,
        state,
        pause_reason_json,
        generation,
        turn,
        max_runs,
        max_total_tokens,
        max_consecutive_failures,
        used_runs,
        used_total_tokens,
        usage_complete,
        consecutive_failures,
        created_at_ms,
        updated_at_ms,
        completed_at_ms,
    ) = row;
    let state = match state.as_str() {
        "running" => StoredGoalState::Running,
        "paused" => StoredGoalState::Paused,
        "completed" => StoredGoalState::Completed,
        _ => return Err(invalid_data("stored goal state is invalid")),
    };
    let pause_reason = pause_reason_json
        .map(|json| {
            serde_json::from_str::<StoredGoalPauseReason>(&json).map_err(|source| {
                invalid_data_with_source("stored goal pause reason is invalid", source)
            })
        })
        .transpose()?;
    let payload = serde_json::from_str::<Vec<StoredGoalObjectivePart>>(&objective_payload_json)
        .map_err(|source| {
            invalid_data_with_source("stored goal objective payload is invalid", source)
        })?;
    Ok(StoredGoal {
        goal_id: GoalId::new(goal_id)
            .map_err(|source| invalid_data_with_source("stored goal id is invalid", source))?,
        session_id: SessionId::new(session_id).map_err(|source| {
            invalid_data_with_source("stored goal session id is invalid", source)
        })?,
        objective: StoredGoalObjective {
            source_message_id: MessageId::new(objective_message_id).map_err(|source| {
                invalid_data_with_source("stored goal objective message id is invalid", source)
            })?,
            payload,
            payload_hash: objective_hash,
        },
        state,
        pause_reason,
        generation: positive_u64(generation, "stored goal generation is invalid")?,
        turn: positive_u32(turn, "stored goal turn is invalid")?,
        budget: StoredGoalBudget {
            max_runs: positive_u32(max_runs, "stored goal run limit is invalid")?,
            max_total_tokens: positive_u64(max_total_tokens, "stored goal token limit is invalid")?,
            max_consecutive_failures: positive_u32(
                max_consecutive_failures,
                "stored goal failure limit is invalid",
            )?,
            used_runs: non_negative_u32(used_runs, "stored goal used runs are invalid")?,
            used_total_tokens: non_negative_u64(
                used_total_tokens,
                "stored goal token usage is invalid",
            )?,
            usage_complete: match usage_complete {
                0 => false,
                1 => true,
                _ => return Err(invalid_data("stored goal usage completeness is invalid")),
            },
        },
        consecutive_failures: non_negative_u32(
            consecutive_failures,
            "stored goal consecutive failures are invalid",
        )?,
        created_at_ms,
        updated_at_ms,
        completed_at_ms,
    })
}

fn positive_u32(value: i64, message: &'static str) -> StorageResult<u32> {
    u32::try_from(positive_u64(value, message)?)
        .map_err(|source| invalid_data_with_source(message, source))
}

fn non_negative_u32(value: i64, message: &'static str) -> StorageResult<u32> {
    u32::try_from(non_negative_u64(value, message)?)
        .map_err(|source| invalid_data_with_source(message, source))
}
