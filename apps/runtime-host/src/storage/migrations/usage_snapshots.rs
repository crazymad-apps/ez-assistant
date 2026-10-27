//! 从已有调用用量账本初始化统计快照；不遍历会话和子任务正文。

use super::{Migration, MigrationError, Result};
use crate::storage::usage::snapshot;
use assistant_protocol::{ChildTaskId, ConversationOwner, SessionId};
use assistant_runtime::StoredConversationUsage;
use rusqlite::Connection;
use std::collections::BTreeMap;

pub(super) fn entry() -> Migration {
    Migration {
        key: "conversation_usage_snapshots_v1",
        version: "0.32.1",
        min_compatible_host_version: Some("0.32.1"),
        rebuilds_referenced_tables: false,
        validate_source: super::f02_history::validate,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(snapshot::SCHEMA)?;
    let mut statement = connection.prepare("SELECT session_id,owner_kind,owner_id,request_kind,input_tokens,output_tokens,total_tokens,cached_input_tokens,reasoning_tokens
        FROM model_request_records ORDER BY completed_at_ms,request_id")?;
    let mut rows = statement.query([])?;
    let mut snapshots = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let key = (
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        );
        let snapshot: &mut StoredConversationUsage = snapshots.entry(key).or_default();
        let usage = agent_types::TokenUsage {
            input_tokens: u64::try_from(row.get::<_, i64>(4)?)
                .map_err(|_| MigrationError::Integrity)?,
            output_tokens: u64::try_from(row.get::<_, i64>(5)?)
                .map_err(|_| MigrationError::Integrity)?,
            total_tokens: u64::try_from(row.get::<_, i64>(6)?)
                .map_err(|_| MigrationError::Integrity)?,
            cached_input_tokens: row
                .get::<_, Option<i64>>(7)?
                .map(u64::try_from)
                .transpose()
                .map_err(|_| MigrationError::Integrity)?,
            reasoning_tokens: row
                .get::<_, Option<i64>>(8)?
                .map(u64::try_from)
                .transpose()
                .map_err(|_| MigrationError::Integrity)?,
        };
        snapshot.accumulate(&usage);
        let kind: String = row.get(3)?;
        snapshot.calibration_pending =
            matches!(kind.as_str(), "context_compaction" | "context_summary");
        snapshot.completed_tokens = (!snapshot.calibration_pending).then_some(usage.total_tokens);
    }
    for ((session, kind, id), usage) in snapshots {
        let session_id = SessionId::new(session).map_err(|_| MigrationError::Integrity)?;
        let owner = if kind == "session" {
            ConversationOwner::MainSession { session_id }
        } else {
            ConversationOwner::ChildTask {
                session_id,
                child_task_id: ChildTaskId::new(id).map_err(|_| MigrationError::Integrity)?,
            }
        };
        snapshot::write(connection, &owner, &usage)?;
    }
    Ok(())
}

fn validate(connection: &Connection) -> Result<()> {
    super::f02_history::validate(connection)?;
    let mut statement =
        connection.prepare("SELECT snapshot_json FROM conversation_usage_snapshots")?;
    for value in statement.query_map([], |row| row.get::<_, String>(0))? {
        let _: StoredConversationUsage = serde_json::from_str(&value?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backfill_uses_existing_ledger_without_any_conversation_files() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE sessions(session_id TEXT PRIMARY KEY);
            INSERT INTO sessions VALUES('owner');
            CREATE TABLE model_request_records(session_id TEXT,owner_kind TEXT,owner_id TEXT,request_kind TEXT,
                input_tokens INTEGER,output_tokens INTEGER,total_tokens INTEGER,cached_input_tokens INTEGER,reasoning_tokens INTEGER,
                completed_at_ms INTEGER,request_id TEXT);
            INSERT INTO model_request_records VALUES
                ('owner','session','owner','agent_turn',100,10,110,20,NULL,1,'a'),
                ('owner','session','owner','context_compaction',40,5,45,NULL,NULL,2,'b'),
                ('owner','child_task','child','agent_turn',200,20,220,0,3,1,'c');").unwrap();
        let transaction = connection.transaction().unwrap();
        apply(&transaction).unwrap();
        transaction.commit().unwrap();
        let parent = snapshot::read(
            &connection,
            &ConversationOwner::MainSession {
                session_id: SessionId::new("owner").unwrap(),
            },
        )
        .unwrap();
        assert_eq!(parent.accumulated.unwrap().total_tokens, 155);
        assert!(parent.calibration_pending);
        assert_eq!(parent.completed_tokens, None);
        let child = snapshot::read(
            &connection,
            &ConversationOwner::ChildTask {
                session_id: SessionId::new("owner").unwrap(),
                child_task_id: ChildTaskId::new("child").unwrap(),
            },
        )
        .unwrap();
        assert_eq!(child.used_tokens(), Some(220));
        assert_eq!(child.accumulated.unwrap().cached_input_tokens, Some(0));
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM conversation_usage_snapshots",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM model_request_records", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            3
        );
    }
}
