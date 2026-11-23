//! 在 v0.33.1 用户库上追加计划定义；数据、会话正文和原有表沿用。

use rusqlite::Connection;

use super::{Migration, MigrationError, Result, v0_33_1};

pub(super) fn entry() -> Migration {
    Migration {
        key: "release",
        version: "0.33.2",
        min_compatible_host_version: Some("0.33.2"),
        rebuilds_referenced_tables: false,
        validate_source: v0_33_1::sidebar_order::entry().validate,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE schedules (
            schedule_id TEXT PRIMARY KEY NOT NULL CHECK(length(trim(schedule_id)) > 0),
            settings_json TEXT NOT NULL CHECK(json_valid(settings_json) AND json_type(settings_json) = 'object')
        ) STRICT",
    )?;
    Ok(())
}

fn validate(connection: &Connection) -> Result<()> {
    (v0_33_1::sidebar_order::entry().validate)(connection)?;
    let invalid = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schedules
            WHERE length(trim(schedule_id)) = 0
               OR CASE WHEN json_valid(settings_json)
                    THEN json_type(settings_json) != 'object' ELSE TRUE END)",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if invalid {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}

/// 已开始使用 M0 的开发库保留原计划，调度器启动时按当前本地日历填充未来到期。
pub(super) fn executions_entry() -> Migration {
    Migration {
        key: "schedule_executions_v1",
        version: "0.33.2",
        min_compatible_host_version: Some("0.33.2"),
        rebuilds_referenced_tables: false,
        validate_source: validate,
        apply: create_executions,
        validate: validate_executions,
    }
}

fn create_executions(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE schedules ADD COLUMN next_due_at_ms INTEGER;
         CREATE INDEX schedules_due ON schedules(next_due_at_ms, schedule_id)
             WHERE next_due_at_ms IS NOT NULL;
         CREATE TABLE schedule_executions (
             execution_id TEXT PRIMARY KEY NOT NULL,
             schedule_id TEXT NOT NULL REFERENCES schedules(schedule_id) ON DELETE CASCADE,
             request_key TEXT,
             scheduled_for_ms INTEGER,
             accepted_at_ms INTEGER NOT NULL,
             finished_at_ms INTEGER,
             status TEXT NOT NULL CHECK(status IN ('running','succeeded','failed','timed_out',
                 'cancelled','skipped','outcome_unknown','cleanup_unconfirmed')),
             settings_json TEXT NOT NULL CHECK(json_valid(settings_json)),
             result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json)),
             CHECK ((request_key IS NULL) != (scheduled_for_ms IS NULL)),
             CHECK ((status = 'running') = (finished_at_ms IS NULL)),
             CHECK ((status = 'running') = (result_json IS NULL)),
             UNIQUE(schedule_id, request_key),
             UNIQUE(schedule_id, scheduled_for_ms)
         ) STRICT;
         CREATE UNIQUE INDEX schedule_executions_active ON schedule_executions(schedule_id)
             WHERE status IN ('running','outcome_unknown','cleanup_unconfirmed');
         CREATE INDEX schedule_executions_history ON schedule_executions(
             schedule_id, accepted_at_ms DESC, execution_id DESC
         );",
    )?;
    Ok(())
}

fn validate_executions(connection: &Connection) -> Result<()> {
    validate(connection)?;
    connection.prepare("SELECT next_due_at_ms FROM schedules LIMIT 0")?;
    connection.prepare(
        "SELECT execution_id, request_key, scheduled_for_ms, accepted_at_ms,
        finished_at_ms, status, settings_json, result_json FROM schedule_executions LIMIT 0",
    )?;
    Ok(())
}

pub(super) fn scheduled_sessions_entry() -> Migration {
    Migration {
        key: "scheduled_sessions_v1",
        version: "0.33.2",
        min_compatible_host_version: Some("0.33.2"),
        rebuilds_referenced_tables: false,
        validate_source: validate_executions,
        apply: bind_scheduled_sessions,
        validate: validate_scheduled_sessions,
    }
}

fn bind_scheduled_sessions(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE schedule_executions ADD COLUMN session_id TEXT
            REFERENCES sessions(session_id) ON DELETE SET NULL;
         CREATE UNIQUE INDEX schedule_execution_owner ON schedule_executions(session_id)
            WHERE session_id IS NOT NULL;",
    )?;
    Ok(())
}

fn validate_scheduled_sessions(connection: &Connection) -> Result<()> {
    validate_executions(connection)?;
    connection.prepare("SELECT session_id FROM schedule_executions LIMIT 0")?;
    Ok(())
}

pub(super) fn schedule_delivery_entry() -> Migration {
    Migration {
        key: "schedule_delivery_v1",
        version: "0.33.2",
        min_compatible_host_version: Some("0.33.2"),
        rebuilds_referenced_tables: false,
        validate_source: validate_scheduled_sessions,
        apply: install_schedule_delivery,
        validate: validate_schedule_delivery,
    }
}

fn install_schedule_delivery(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE schedules ADD COLUMN deleting INTEGER NOT NULL DEFAULT 0 CHECK(deleting IN (0,1));
         ALTER TABLE schedule_executions ADD COLUMN files_json TEXT
            CHECK(files_json IS NULL OR json_valid(files_json));
         ALTER TABLE schedule_executions ADD COLUMN result_finalized INTEGER NOT NULL DEFAULT 0
            CHECK(result_finalized IN (0,1));
         ALTER TABLE schedule_executions ADD COLUMN tracks_script_process INTEGER NOT NULL DEFAULT 0
            CHECK(tracks_script_process IN (0,1));
         ALTER TABLE schedule_executions ADD COLUMN cleanup_confirmed INTEGER NOT NULL DEFAULT 0
            CHECK(cleanup_confirmed IN (0,1));
         DROP INDEX schedule_executions_active;
         CREATE UNIQUE INDEX schedule_executions_active ON schedule_executions(schedule_id)
            WHERE status IN ('running','outcome_unknown','cleanup_unconfirmed') AND cleanup_confirmed=0;
         CREATE INDEX schedules_deleting ON schedules(schedule_id) WHERE deleting=1;
         CREATE INDEX schedule_results_pending ON schedule_executions(execution_id)
            WHERE result_finalized=0 AND status<>'running';
         CREATE TABLE schedule_deliveries (
            execution_id TEXT PRIMARY KEY NOT NULL,
            session_id TEXT NOT NULL REFERENCES sessions(session_id),
            message_id TEXT NOT NULL UNIQUE,
            files_json TEXT NOT NULL CHECK(json_valid(files_json))
         ) STRICT;",
    )?;
    Ok(())
}

fn validate_schedule_delivery(connection: &Connection) -> Result<()> {
    validate_scheduled_sessions(connection)?;
    connection.prepare("SELECT deleting FROM schedules LIMIT 0")?;
    connection.prepare("SELECT files_json,result_finalized,tracks_script_process,cleanup_confirmed FROM schedule_executions LIMIT 0")?;
    connection.prepare(
        "SELECT execution_id,session_id,message_id,files_json FROM schedule_deliveries LIMIT 0",
    )?;
    Ok(())
}
