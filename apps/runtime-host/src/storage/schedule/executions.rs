//! 接纳事务同时消费定时游标、冻结配置并占用该计划的执行名额。

use assistant_protocol::{
    ContextPage, ListScheduleExecutionsResult, ScheduleAction, ScheduleAgentResult,
    ScheduleExecution, ScheduleExecutionId, ScheduleExecutionReason, ScheduleExecutionStatus,
    ScheduleExecutionSummary, ScheduleId, ScheduleScriptResult, SessionId,
};
use assistant_runtime::{
    ScheduleAdmission, ScheduleAdmitted, ScheduleResult, ScheduleSettlement, ScheduleTrigger,
    StoredSchedule,
};
use rusqlite::{Connection, OptionalExtension as _, Row, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, database_write_error, decode, encode, internal_error,
    invalid_data_with_source,
};

const SUMMARY: &str =
    "execution_id,schedule_id,accepted_at_ms,scheduled_for_ms,finished_at_ms,status";
const OCCUPIED: &str =
    "status IN ('running','outcome_unknown','cleanup_unconfirmed') AND cleanup_confirmed=0";

impl StorageEngine {
    pub(in crate::storage) fn next_schedule_due(&self) -> StorageResult<Option<StoredSchedule>> {
        let raw = self
            .connection
            .query_row(
                "SELECT schedule_id,settings_json,next_due_at_ms FROM schedules
             WHERE deleting=0 AND next_due_at_ms IS NOT NULL ORDER BY next_due_at_ms,schedule_id LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get(2)?,
                    ))
                },
            )
            .optional()
            .map_err(read_error)?;
        raw.map(|(id, settings, next_due_at_ms)| {
            let id = ScheduleId::new(id)
                .map_err(|error| invalid_data_with_source("invalid schedule id", error))?;
            decode(id, &settings, next_due_at_ms, false)
        })
        .transpose()
    }

    /// 事务先复核当前计划和触发身份，再推进游标与插入记录。可靠提交后 Runtime 才能启动进程。
    /// 重复请求回读既有事实。中途失败回滚游标与记录，存储线程不创建进程。
    pub(in crate::storage) fn admit_schedule(
        &mut self,
        command: ScheduleAdmission,
    ) -> StorageResult<ScheduleAdmitted> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(write_error)?;
        let schedule_id = command.schedule_id.as_str();
        let raw = tx
            .query_row(
                "SELECT settings_json,next_due_at_ms FROM schedules WHERE schedule_id=?1 AND deleting=0",
                [schedule_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()
            .map_err(read_error)?;
        let Some((settings, expected_due)) = raw else {
            return Ok(ScheduleAdmitted::Missing);
        };
        let (key, due) = match &command.trigger {
            ScheduleTrigger::Manual(key) => (Some(key.as_str()), None),
            ScheduleTrigger::Due { at_ms, .. } => (None, Some(*at_ms)),
        };
        let sql = format!(
            "SELECT {SUMMARY} FROM schedule_executions
            WHERE schedule_id=?1 AND (request_key=?2 OR scheduled_for_ms=?3)"
        );
        let existing = tx
            .query_row(&sql, params![schedule_id, key, due], read_summary)
            .optional()
            .map_err(read_error)?;
        if due.is_none()
            && let Some(execution) = existing
        {
            return Ok(ScheduleAdmitted::Replayed(execution));
        }
        let schedule = decode(command.schedule_id.clone(), &settings, expected_due, false)?;
        if let ScheduleTrigger::Due {
            at_ms,
            next_at_ms,
            missed,
        } = command.trigger
        {
            if !schedule.settings.configuration.enabled || expected_due != Some(at_ms) {
                return Ok(ScheduleAdmitted::Ignored);
            }
            tx.execute(
                "UPDATE schedules SET next_due_at_ms=?2 WHERE schedule_id=?1",
                params![schedule_id, next_at_ms],
            )
            .map_err(write_error)?;
            if missed || existing.is_some() {
                tx.commit().map_err(write_error)?;
                return Ok(ScheduleAdmitted::Ignored);
            }
        }
        let active = active(&tx, &command.schedule_id)?;
        if due.is_none()
            && let Some(execution) = active
        {
            return Ok(ScheduleAdmitted::Busy(execution));
        }

        let delivered_id_exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schedule_deliveries WHERE execution_id=?1)",
                [command.execution_id.as_str()],
                |row| row.get(0),
            )
            .map_err(read_error)?;
        if delivered_id_exists {
            return Err(crate::storage::conflict(
                "schedule execution id is already owned by a delivery",
            ));
        }
        let execution = command.execution(schedule.settings, active.is_some());
        let row = &execution.summary;
        tx.execute(
            "INSERT INTO schedule_executions(execution_id,schedule_id,request_key,scheduled_for_ms,
                accepted_at_ms,finished_at_ms,status,settings_json,result_json,tracks_script_process)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                row.execution_id.as_str(),
                schedule_id,
                key,
                due,
                row.accepted_at_ms,
                row.finished_at_ms,
                status_code(row.status),
                encode(&execution.settings)?,
                execution
                    .script_result
                    .as_ref()
                    .map(encode_result)
                    .or_else(|| execution.agent_result.as_ref().map(encode_result))
                    .transpose()?,
                command.tracks_script_process && matches!(execution.settings.configuration.action, ScheduleAction::Script { .. }),
            ],
        )
        .map_err(write_error)?;
        tx.commit().map_err(write_error)?;
        Ok(if row.status == ScheduleExecutionStatus::Running {
            ScheduleAdmitted::Started(Box::new(execution))
        } else {
            ScheduleAdmitted::Ignored
        })
    }

    pub(in crate::storage) fn settle_schedule_execution(
        &self,
        settlement: ScheduleSettlement,
    ) -> StorageResult<()> {
        self.connection.execute(
            "UPDATE schedule_executions SET finished_at_ms=max(?2,accepted_at_ms),status=?3,result_json=?4
             WHERE execution_id=?1 AND status='running'",
            params![settlement.execution_id.as_str(), settlement.finished_at_ms, status_code(settlement.status),
                match &settlement.result {
                    ScheduleResult::Script(result) => encode_result(result)?,
                    ScheduleResult::Agent(result) => encode_result(result)?,
                }],
        ).map_err(write_error)?;
        Ok(())
    }

    /// 崩溃时留下的 running 只能标记未知。保留名额，避免和可能仍存活的进程叠跑。
    pub(in crate::storage) fn recover_schedule_executions(
        &self,
        now: i64,
        cleanup_verified: bool,
    ) -> StorageResult<()> {
        let result = ScheduleScriptResult {
            reason: Some(ScheduleExecutionReason::HostInterrupted),
            ..Default::default()
        };
        let agent = ScheduleAgentResult {
            reason: result.reason,
            ..Default::default()
        };
        self.connection.execute(
            "UPDATE schedule_executions SET finished_at_ms=max(?1,accepted_at_ms),status='outcome_unknown',
             result_json=CASE WHEN json_extract(settings_json,'$.action.type')='agent' THEN ?3 ELSE ?2 END
             WHERE status='running'", params![now, encode_result(&result)?, encode_result(&agent)?],
        ).map_err(write_error)?;
        if cleanup_verified {
            self.connection.execute(
                "UPDATE schedule_executions SET cleanup_confirmed=1
                 WHERE tracks_script_process=1 AND status IN ('outcome_unknown','cleanup_unconfirmed')",
                [],
            ).map_err(write_error)?;
        }
        Ok(())
    }

    pub(in crate::storage) fn get_schedule_execution(
        &self,
        id: ScheduleExecutionId,
    ) -> StorageResult<Option<ScheduleExecution>> {
        let sql = format!(
            "SELECT {SUMMARY},settings_json,result_json,session_id,files_json,result_finalized FROM schedule_executions WHERE execution_id=?1"
        );
        let row = self
            .connection
            .query_row(&sql, [id.as_str()], |row| {
                Ok((
                    read_summary(row)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, bool>(10)?,
                ))
            })
            .optional()
            .map_err(read_error)?;
        row.map(
            |(summary, settings, result, session_id, files, result_finalized)| {
                let settings =
                    decode(summary.schedule_id.clone(), &settings, None, false)?.settings;
                let mut execution = ScheduleExecution {
                    summary,
                    settings,
                    script_result: None,
                    agent_result: None,
                    agent_session_id: session_id.map(SessionId::new).transpose().map_err(
                        |error| invalid_data_with_source("invalid scheduled session id", error),
                    )?,
                    agent_run_status: None,
                    background_runs: Vec::new(),
                    files: files
                        .as_deref()
                        .map(super::delivery::decode_files)
                        .transpose()?
                        .unwrap_or_default(),
                    delivery: None,
                    result_finalized,
                    result_error: None,
                };
                if let Some(result) = result {
                    match execution.settings.configuration.action {
                        ScheduleAction::Script { .. } => {
                            execution.script_result = Some(decode_result(&result)?)
                        }
                        ScheduleAction::Agent { .. } => {
                            execution.agent_result = Some(decode_result(&result)?)
                        }
                    }
                }
                execution.delivery = self
                    .schedule_delivery(&execution.summary.execution_id)?
                    .map(|(location, _)| location);
                if execution.agent_session_id.is_none() {
                    execution.agent_run_status = execution
                        .agent_result
                        .as_ref()
                        .and_then(|result| result.run_status);
                }
                Ok(execution)
            },
        )
        .transpose()
    }

    pub(in crate::storage) fn list_schedule_executions(
        &self,
        id: ScheduleId,
        offset: u32,
        limit: u32,
    ) -> StorageResult<Option<ListScheduleExecutionsResult>> {
        let exists = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schedules WHERE schedule_id=?1)",
                [id.as_str()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(read_error)?;
        if !exists {
            return Ok(None);
        }
        let total: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM schedule_executions WHERE schedule_id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .map_err(read_error)?;
        let sql = format!(
            "SELECT {SUMMARY} FROM schedule_executions WHERE schedule_id=?1
            ORDER BY accepted_at_ms DESC,execution_id DESC LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = self.connection.prepare(&sql).map_err(read_error)?;
        let executions = stmt
            .query_map(params![id.as_str(), limit, offset], read_summary)
            .map_err(read_error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(read_error)?;
        Ok(Some(ListScheduleExecutionsResult {
            executions,
            page: ContextPage {
                offset,
                limit,
                total: u32::try_from(total).unwrap_or(u32::MAX),
            },
        }))
    }
}

fn active(
    connection: &Connection,
    id: &ScheduleId,
) -> StorageResult<Option<ScheduleExecutionSummary>> {
    let sql =
        format!("SELECT {SUMMARY} FROM schedule_executions WHERE schedule_id=?1 AND {OCCUPIED}");
    connection
        .query_row(&sql, [id.as_str()], read_summary)
        .optional()
        .map_err(read_error)
}

fn read_summary(row: &Row<'_>) -> rusqlite::Result<ScheduleExecutionSummary> {
    Ok(ScheduleExecutionSummary {
        execution_id: ScheduleExecutionId::new(row.get::<_, String>(0)?)
            .map_err(|error| from_sql(0, error))?,
        schedule_id: ScheduleId::new(row.get::<_, String>(1)?)
            .map_err(|error| from_sql(1, error))?,
        accepted_at_ms: row.get(2)?,
        scheduled_for_ms: row.get(3)?,
        finished_at_ms: row.get(4)?,
        status: serde_json::from_value(serde_json::Value::String(row.get(5)?))
            .map_err(|error| from_sql(5, error))?,
    })
}

fn from_sql(
    column: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(error))
}

fn decode_result<T: serde::de::DeserializeOwned>(result: &str) -> StorageResult<T> {
    serde_json::from_str(result)
        .map_err(|error| invalid_data_with_source("invalid schedule result", error))
}

fn encode_result(result: &impl serde::Serialize) -> StorageResult<String> {
    serde_json::to_string(result)
        .map_err(|error| invalid_data_with_source("invalid schedule result", error))
}

fn status_code(status: ScheduleExecutionStatus) -> &'static str {
    use ScheduleExecutionStatus::*;
    match status {
        Running => "running",
        Succeeded => "succeeded",
        Failed => "failed",
        TimedOut => "timed_out",
        Cancelled => "cancelled",
        Skipped => "skipped",
        OutcomeUnknown => "outcome_unknown",
        CleanupUnconfirmed => "cleanup_unconfirmed",
    }
}

fn read_error(error: rusqlite::Error) -> assistant_runtime::StoreError {
    internal_error("schedule execution could not be read", error)
}

fn write_error(error: rusqlite::Error) -> assistant_runtime::StoreError {
    database_write_error("schedule execution could not be committed", error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_protocol::IdempotencyKey;

    #[test]
    fn shared_admission_records_overlap_and_replays_only_existing_receipts() {
        let home = tempfile::tempdir().unwrap();
        let mut db = StorageEngine::open(home.path()).unwrap();
        let schedule = ScheduleId::new("schedule_fixture").unwrap();
        let settings = serde_json::from_value(serde_json::json!({
            "name":"fixture","enabled":true,"report_to_controller":false,
            "rule":{"type":"daily","time":"09:00"},
            "action":{"type":"script","command":"echo fixture"}
        }))
        .unwrap();
        assert!(
            db.insert_schedule(StoredSchedule {
                deleting: false,
                once_triggered: false,
                schedule_id: schedule.clone(),
                settings,
                next_due_at_ms: Some(2000),
            })
            .unwrap()
        );
        let admit = |db: &mut StorageEngine, id, now, trigger| {
            db.admit_schedule(ScheduleAdmission {
                schedule_id: schedule.clone(),
                execution_id: ScheduleExecutionId::new(id).unwrap(),
                accepted_at_ms: now,
                tracks_script_process: false,
                trigger,
            })
            .unwrap()
        };
        let manual = || ScheduleTrigger::Manual(IdempotencyKey::new("request-1").unwrap());
        let ScheduleAdmitted::Started(first) = admit(&mut db, "first", 1000, manual()) else {
            panic!("manual script was not admitted")
        };
        assert_eq!(first.summary.scheduled_for_ms, None);
        assert_eq!(
            db.get_schedule(schedule.clone())
                .unwrap()
                .unwrap()
                .next_due_at_ms,
            Some(2000)
        );
        assert!(
            matches!(admit(&mut db, "retry", 1001, manual()), ScheduleAdmitted::Replayed(known) if known.execution_id == first.summary.execution_id)
        );
        assert!(
            matches!(admit(&mut db, "busy", 1002, ScheduleTrigger::Manual(IdempotencyKey::new("another").unwrap())),
            ScheduleAdmitted::Busy(known) if known.execution_id == first.summary.execution_id)
        );

        let scheduled = ScheduleTrigger::Due {
            at_ms: 2000,
            next_at_ms: Some(5000),
            missed: false,
        };
        assert!(matches!(
            admit(&mut db, "overlap", 2001, scheduled.clone()),
            ScheduleAdmitted::Ignored
        ));
        assert!(matches!(
            admit(&mut db, "duplicate", 2002, scheduled),
            ScheduleAdmitted::Ignored
        ));
        let overlap = db
            .get_schedule_execution(ScheduleExecutionId::new("overlap").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(overlap.summary.status, ScheduleExecutionStatus::Skipped);
        assert_eq!(overlap.summary.scheduled_for_ms, Some(2000));
        assert_eq!(
            overlap.script_result.unwrap().reason,
            Some(ScheduleExecutionReason::PreviousExecutionActive)
        );

        let missed = ScheduleTrigger::Due {
            at_ms: 5000,
            next_at_ms: Some(12000),
            missed: true,
        };
        assert!(matches!(
            admit(&mut db, "missed", 11000, missed),
            ScheduleAdmitted::Ignored
        ));
        assert_eq!(
            db.get_schedule(schedule.clone())
                .unwrap()
                .unwrap()
                .next_due_at_ms,
            Some(12000)
        );
        assert_eq!(
            db.list_schedule_executions(schedule.clone(), 0, 10)
                .unwrap()
                .unwrap()
                .page
                .total,
            2
        );

        db.recover_schedule_executions(11_500, false).unwrap();
        let recovered = db
            .get_schedule_execution(first.summary.execution_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            recovered.summary.status,
            ScheduleExecutionStatus::OutcomeUnknown
        );
        assert_eq!(
            recovered.script_result.unwrap().reason,
            Some(ScheduleExecutionReason::HostInterrupted)
        );
        assert!(!db.delete_schedule(schedule).unwrap());
    }
}
