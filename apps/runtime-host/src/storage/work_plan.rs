//! Session WorkPlan 的 SQLite CAS、幂等写入与恢复投影。

use assistant_protocol::{SessionId, TodoItemId};
use assistant_runtime::{
    StoredTodoItemStatus, StoredWorkPlan, StoredWorkPlanItem, WorkPlanClear, WorkPlanMutation,
    WorkPlanMutationResult,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, internal_error, invalid_data,
    invalid_data_with_source, positive_u64, to_i64,
};

impl StorageEngine {
    pub(super) fn load_all_work_plans(&self) -> StorageResult<Vec<StoredWorkPlan>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT session_id, revision, objective, items_json, last_operation_id,
                        updated_at_ms
                 FROM session_work_plans
                 ORDER BY session_id",
            )
            .map_err(|source| internal_error("work plans could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|source| internal_error("work plans could not be read", source))?;
        let mut plans = Vec::new();
        for row in rows {
            let (session_id, revision, objective, items_json, last_operation_id, updated_at_ms) =
                row.map_err(|source| internal_error("work plan row could not be read", source))?;
            plans.push(parse_work_plan(
                session_id,
                revision,
                objective,
                items_json,
                last_operation_id,
                updated_at_ms,
            )?);
        }
        Ok(plans)
    }

    pub(super) fn load_work_plan(
        &self,
        session_id: &SessionId,
    ) -> StorageResult<Option<StoredWorkPlan>> {
        self.connection
            .query_row(
                "SELECT session_id, revision, objective, items_json, last_operation_id,
                        updated_at_ms
                 FROM session_work_plans WHERE session_id = ?1",
                [session_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("work plan could not be queried", source))?
            .map(
                |(session_id, revision, objective, items_json, operation_id, updated_at_ms)| {
                    parse_work_plan(
                        session_id,
                        revision,
                        objective,
                        items_json,
                        operation_id,
                        updated_at_ms,
                    )
                },
            )
            .transpose()
    }

    pub(super) fn mutate_work_plan(
        &mut self,
        mutation: WorkPlanMutation,
    ) -> StorageResult<WorkPlanMutationResult> {
        let items_json = serde_json::to_string(&mutation.items)
            .map_err(|source| internal_error("work plan items could not be encoded", source))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("work plan transaction could not be started", source)
            })?;
        let lifecycle = transaction
            .query_row(
                "SELECT lifecycle FROM sessions WHERE session_id = ?1",
                [mutation.session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("work plan session could not be queried", source))?
            .ok_or_else(|| conflict("work plan session does not exist"))?;
        if lifecycle != "active" {
            return Err(conflict("work plan session is archived"));
        }
        let completion_receipt = transaction
            .query_row(
                "SELECT revision, objective, items_json, updated_at_ms
                 FROM work_plan_completion_receipts
                 WHERE session_id = ?1 AND operation_id = ?2",
                params![mutation.session_id.as_str(), mutation.operation_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| {
                internal_error("work plan completion receipt could not be queried", source)
            })?;
        if let Some((revision, objective, items_json, updated_at_ms)) = completion_receipt {
            return Ok(WorkPlanMutationResult {
                plan: parse_work_plan(
                    mutation.session_id.as_str().to_owned(),
                    revision,
                    objective,
                    items_json,
                    mutation.operation_id,
                    updated_at_ms,
                )?,
                cleared: true,
            });
        }
        let current = transaction
            .query_row(
                "SELECT revision, objective, items_json, last_operation_id, updated_at_ms
                 FROM session_work_plans WHERE session_id = ?1",
                [mutation.session_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| internal_error("current work plan could not be queried", source))?;
        if let Some((revision, objective, items_json, operation_id, updated_at_ms)) = current {
            if operation_id == mutation.operation_id {
                return Ok(WorkPlanMutationResult {
                    plan: parse_work_plan(
                        mutation.session_id.as_str().to_owned(),
                        revision,
                        objective,
                        items_json,
                        operation_id,
                        updated_at_ms,
                    )?,
                    cleared: false,
                });
            }
            if positive_u64(revision, "stored work plan revision is invalid")?
                != mutation.expected_revision
            {
                return Err(conflict("work plan revision changed"));
            }
        } else if mutation.expected_revision != 0 {
            return Err(conflict("work plan revision changed"));
        }
        let revision = mutation
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| conflict("work plan revision exhausted"))?;
        let revision_sql = to_i64(revision, "work plan revision exceeds storage range")?;
        let completed = !mutation.items.is_empty()
            && mutation
                .items
                .iter()
                .all(|item| item.status == StoredTodoItemStatus::Completed);
        if completed {
            transaction
                .execute(
                    "INSERT INTO work_plan_completion_receipts (
                        session_id, operation_id, revision, objective, items_json, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        mutation.session_id.as_str(),
                        mutation.operation_id,
                        revision_sql,
                        mutation.objective,
                        items_json,
                        mutation.updated_at_ms,
                    ],
                )
                .map_err(|source| {
                    database_write_error("work plan completion receipt could not be stored", source)
                })?;
            transaction
                .execute(
                    "DELETE FROM session_work_plans WHERE session_id = ?1",
                    [mutation.session_id.as_str()],
                )
                .map_err(|source| {
                    database_write_error("completed work plan could not be cleared", source)
                })?;
        } else {
            transaction
                .execute(
                    "INSERT INTO session_work_plans (
                    session_id, revision, objective, items_json, last_operation_id, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(session_id) DO UPDATE SET
                    revision = excluded.revision,
                    objective = excluded.objective,
                    items_json = excluded.items_json,
                    last_operation_id = excluded.last_operation_id,
                    updated_at_ms = excluded.updated_at_ms",
                    params![
                        mutation.session_id.as_str(),
                        revision_sql,
                        mutation.objective,
                        items_json,
                        mutation.operation_id,
                        mutation.updated_at_ms,
                    ],
                )
                .map_err(|source| database_write_error("work plan could not be stored", source))?;
        }
        transaction.commit().map_err(|source| {
            database_write_error("work plan transaction could not be committed", source)
        })?;
        Ok(WorkPlanMutationResult {
            plan: StoredWorkPlan {
                session_id: mutation.session_id,
                revision,
                objective: mutation.objective,
                items: mutation.items,
                last_operation_id: mutation.operation_id,
                updated_at_ms: mutation.updated_at_ms,
            },
            cleared: completed,
        })
    }

    pub(super) fn clear_work_plan(&mut self, clear: WorkPlanClear) -> StorageResult<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("work plan clear transaction could not be started", source)
            })?;
        let lifecycle = transaction
            .query_row(
                "SELECT lifecycle FROM sessions WHERE session_id = ?1",
                [clear.session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| internal_error("work plan session could not be queried", source))?
            .ok_or_else(|| conflict("work plan session does not exist"))?;
        if lifecycle != "active" {
            return Err(conflict("work plan session is archived"));
        }
        let current = transaction
            .query_row(
                "SELECT revision FROM session_work_plans WHERE session_id = ?1",
                [clear.session_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|source| internal_error("work plan revision could not be queried", source))?;
        match current {
            Some(revision)
                if positive_u64(revision, "stored work plan revision is invalid")?
                    == clear.expected_revision => {}
            None if clear.expected_revision == 0 => return Ok(()),
            _ => return Err(conflict("work plan revision changed")),
        }
        transaction
            .execute(
                "DELETE FROM session_work_plans WHERE session_id = ?1",
                [clear.session_id.as_str()],
            )
            .map_err(|source| database_write_error("work plan could not be cleared", source))?;
        transaction.commit().map_err(|source| {
            database_write_error("work plan clear transaction could not be committed", source)
        })?;
        Ok(())
    }
}

fn parse_work_plan(
    session_id: String,
    revision: i64,
    objective: String,
    items_json: String,
    last_operation_id: String,
    updated_at_ms: i64,
) -> StorageResult<StoredWorkPlan> {
    let session_id = SessionId::new(session_id).map_err(|source| {
        invalid_data_with_source("stored work plan session id is invalid", source)
    })?;
    if last_operation_id.trim().is_empty() {
        return Err(invalid_data("stored work plan operation id is invalid"));
    }
    let items = serde_json::from_str::<Vec<StoredWorkPlanItem>>(&items_json)
        .map_err(|source| invalid_data_with_source("stored work plan items are invalid", source))?;
    let mut ids = std::collections::BTreeSet::<TodoItemId>::new();
    if items.iter().any(|item| !ids.insert(item.id.clone())) {
        return Err(invalid_data("stored work plan item ids are duplicated"));
    }
    Ok(StoredWorkPlan {
        session_id,
        revision: positive_u64(revision, "stored work plan revision is invalid")?,
        objective,
        items,
        last_operation_id,
        updated_at_ms,
    })
}
