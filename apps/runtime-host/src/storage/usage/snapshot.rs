//! 主会话和子任务共用统计快照。提交事务推进数值；展示不扫描 JSONL。

use agent_types::ConversationMessage;
use assistant_protocol::ConversationOwner;
use assistant_runtime::StoredConversationUsage;
use rusqlite::{Connection, OptionalExtension, params};

use super::super::{StorageEngine, StorageResult, internal_error};

pub(crate) const SCHEMA: &str = "CREATE TABLE conversation_usage_snapshots (
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    owner_kind TEXT NOT NULL CHECK(owner_kind IN ('session','child_task')),
    owner_id TEXT NOT NULL,
    snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json)),
    PRIMARY KEY(session_id,owner_kind,owner_id)
)";

fn key(owner: &ConversationOwner) -> (&str, &str, &str) {
    match owner {
        ConversationOwner::MainSession { session_id } => {
            (session_id.as_str(), "session", session_id.as_str())
        }
        ConversationOwner::ChildTask {
            session_id,
            child_task_id,
        } => (session_id.as_str(), "child_task", child_task_id.as_str()),
    }
}

pub(super) fn exists(connection: &Connection) -> StorageResult<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='conversation_usage_snapshots')",
            [],
            |row| row.get(0),
        )
        .map_err(|source| internal_error("usage snapshot schema could not be read", source))
}

pub(crate) fn read(
    connection: &Connection,
    owner: &ConversationOwner,
) -> StorageResult<StoredConversationUsage> {
    let (session, kind, id) = key(owner);
    let json: Option<String> = connection.query_row(
        "SELECT snapshot_json FROM conversation_usage_snapshots WHERE session_id=?1 AND owner_kind=?2 AND owner_id=?3",
        params![session, kind, id], |row| row.get(0))
        .optional().map_err(|source| internal_error("usage snapshot could not be read", source))?;
    json.map(|json| {
        serde_json::from_str(&json)
            .map_err(|source| internal_error("usage snapshot is invalid", source))
    })
    .transpose()
    .map(|usage| usage.unwrap_or_default())
}

pub(crate) fn write(
    connection: &Connection,
    owner: &ConversationOwner,
    usage: &StoredConversationUsage,
) -> StorageResult<()> {
    let (session, kind, id) = key(owner);
    let json = serde_json::to_string(usage)
        .map_err(|source| internal_error("usage snapshot could not be encoded", source))?;
    connection.execute("INSERT INTO conversation_usage_snapshots(session_id,owner_kind,owner_id,snapshot_json)
        VALUES(?1,?2,?3,?4) ON CONFLICT(session_id,owner_kind,owner_id) DO UPDATE SET snapshot_json=excluded.snapshot_json",
        params![session, kind, id, json]).map_err(|source| internal_error("usage snapshot could not be saved", source))?;
    Ok(())
}

pub(in crate::storage) fn clear(
    connection: &Connection,
    session_id: &assistant_protocol::SessionId,
) -> StorageResult<()> {
    if exists(connection)? {
        connection
            .execute(
                "DELETE FROM conversation_usage_snapshots WHERE session_id=?1",
                [session_id.as_str()],
            )
            .map_err(|source| internal_error("usage snapshots could not be cleared", source))?;
    }
    Ok(())
}

pub(super) fn append_context(
    connection: &Connection,
    owner: &ConversationOwner,
    messages: &[&ConversationMessage],
) -> StorageResult<()> {
    if !exists(connection)? {
        return Ok(());
    }
    let mut usage = read(connection, owner)?;
    for message in messages {
        usage.append_context(message);
    }
    write(connection, owner, &usage)
}

pub(super) fn accumulate(
    connection: &Connection,
    owner: &ConversationOwner,
    tokens: &agent_types::TokenUsage,
) -> StorageResult<()> {
    if !exists(connection)? {
        return Ok(());
    }
    let mut usage = read(connection, owner)?;
    usage.accumulate(tokens);
    write(connection, owner, &usage)
}

pub(in crate::storage) fn replace_context(
    connection: &Connection,
    owner: &ConversationOwner,
    snapshot: &agent_types::ConversationSnapshot,
) -> StorageResult<()> {
    if !exists(connection)? {
        return Ok(());
    }
    let mut usage = read(connection, owner)?;
    let window = snapshot
        .project_provider_context()
        .map_err(|source| internal_error("usage context is invalid", source))?;
    usage.replace_context(&window);
    write(connection, owner, &usage)
}

impl StorageEngine {
    pub(in crate::storage) fn get_conversation_usage(
        &self,
        owner: &ConversationOwner,
    ) -> StorageResult<StoredConversationUsage> {
        read(&self.connection, owner)
    }
}
