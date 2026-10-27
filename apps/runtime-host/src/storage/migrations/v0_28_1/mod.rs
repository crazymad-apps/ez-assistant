//! F04 问答有独立的待回答生命周期；放入既有会话库并由 Session/Run 外键管理清理。
use super::{Migration, Result, v0_28_0};
use rusqlite::Connection;

pub(super) fn entry() -> Migration {
    Migration {
        key: "release",
        version: "0.28.1",
        min_compatible_host_version: Some("0.28.1"),
        rebuilds_referenced_tables: false,
        validate_source: v0_28_0::validate,
        apply,
        validate,
    }
}
fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE session_feedback (
        request_id TEXT PRIMARY KEY NOT NULL,
        session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
        run_id TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
        snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json) AND json_type(snapshot_json)='object')
    );
    CREATE INDEX session_feedback_session ON session_feedback(session_id);
    CREATE UNIQUE INDEX session_feedback_pending ON session_feedback(session_id)
        WHERE json_extract(snapshot_json,'$.status')='pending';")?;
    Ok(())
}
pub(super) fn validate(connection: &Connection) -> Result<()> {
    v0_28_0::validate(connection)?;
    validate_additions(connection)
}

pub(super) fn validate_additions(connection: &Connection) -> Result<()> {
    connection.prepare(
        "SELECT request_id, session_id, run_id, snapshot_json FROM session_feedback LIMIT 0",
    )?;
    Ok(())
}
