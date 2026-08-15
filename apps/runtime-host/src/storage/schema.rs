//! SQLite schema 初始化与当前格式核验。

use rusqlite::{Connection, TransactionBehavior};

use super::{StorageResult, internal_error};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    session_id          TEXT PRIMARY KEY,
    title               TEXT NOT NULL,
    model_key           TEXT NOT NULL,
    system_prompt_json  TEXT NOT NULL,
    current_variant     TEXT NOT NULL DEFAULT 'build'
                            CHECK (current_variant IN ('plan', 'build')),
    approval_mode       TEXT NOT NULL DEFAULT 'ask'
                            CHECK (approval_mode IN ('ask', 'auto')),
    lifecycle           TEXT NOT NULL CHECK (lifecycle IN ('active', 'archived')),
    body_generation     INTEGER NOT NULL CHECK (body_generation > 0),
    message_count       INTEGER NOT NULL CHECK (message_count >= 0),
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL,
    archived_at_ms      INTEGER,
    is_pinned           INTEGER NOT NULL DEFAULT 0 CHECK (is_pinned IN (0, 1)),
    title_origin        TEXT NOT NULL DEFAULT 'generated'
                            CHECK (title_origin IN ('generated', 'user'))
);

CREATE TABLE IF NOT EXISTS message_feedback (
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    message_id          TEXT NOT NULL,
    feedback            TEXT NOT NULL CHECK (feedback IN ('positive', 'negative')),
    changed_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (session_id, message_id)
);

CREATE TABLE IF NOT EXISTS workspaces (
    workspace_id       TEXT PRIMARY KEY,
    user_directory     TEXT NOT NULL UNIQUE,
    agent_directory    TEXT NOT NULL UNIQUE,
    lifecycle          TEXT NOT NULL CHECK (lifecycle IN ('active', 'removed')),
    created_at_ms      INTEGER NOT NULL,
    updated_at_ms      INTEGER NOT NULL,
    removed_at_ms      INTEGER
);

CREATE TABLE IF NOT EXISTS session_resources (
    session_id             TEXT PRIMARY KEY REFERENCES sessions(session_id) ON DELETE CASCADE,
    workspace_id           TEXT REFERENCES workspaces(workspace_id),
    working_directory      TEXT NOT NULL,
    attachment_directory   TEXT NOT NULL UNIQUE,
    private_directory      TEXT NOT NULL UNIQUE,
    created_at_ms          INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS session_resources_workspace
    ON session_resources(workspace_id, session_id);

CREATE TABLE IF NOT EXISTS attachment_blobs (
    blob_hash          TEXT PRIMARY KEY,
    size_bytes         INTEGER NOT NULL CHECK (size_bytes >= 0),
    relative_path      TEXT NOT NULL UNIQUE,
    created_at_ms      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attachments (
    attachment_id          TEXT PRIMARY KEY,
    session_id             TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    blob_hash              TEXT NOT NULL REFERENCES attachment_blobs(blob_hash),
    original_name          TEXT NOT NULL,
    agent_readable_path    TEXT NOT NULL UNIQUE,
    state                  TEXT NOT NULL CHECK (state IN ('ready', 'unavailable')),
    created_at_ms          INTEGER NOT NULL,
    UNIQUE (session_id, blob_hash)
);

CREATE INDEX IF NOT EXISTS attachments_session_order
    ON attachments(session_id, created_at_ms, attachment_id);

CREATE TABLE IF NOT EXISTS inputs (
    queue_order         INTEGER PRIMARY KEY AUTOINCREMENT,
    priority_order      INTEGER,
    input_id            TEXT NOT NULL UNIQUE,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    idempotency_key     TEXT,
    user_message_id     TEXT NOT NULL UNIQUE,
    state               TEXT NOT NULL CHECK (state IN ('queued', 'committed')),
    queued_message_json TEXT,
    accepted_at_ms      INTEGER NOT NULL,
    agent_variant       TEXT NOT NULL DEFAULT 'build'
                            CHECK (agent_variant IN ('plan', 'build')),
    UNIQUE (session_id, idempotency_key)
);

CREATE TABLE IF NOT EXISTS runs (
    run_id              TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    input_id            TEXT NOT NULL REFERENCES inputs(input_id) ON DELETE CASCADE,
    attempt             INTEGER NOT NULL CHECK (attempt > 0),
    status              TEXT NOT NULL,
    cancel_requested    INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
    approval_mode       TEXT NOT NULL DEFAULT 'ask'
                            CHECK (approval_mode IN ('ask', 'auto')),
    error_code          TEXT,
    error_message       TEXT,
    created_at_ms       INTEGER NOT NULL,
    started_at_ms       INTEGER,
    finished_at_ms      INTEGER,
    UNIQUE (input_id, attempt)
);

CREATE TABLE IF NOT EXISTS run_message_refs (
    run_id              TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    message_id          TEXT NOT NULL,
    PRIMARY KEY (run_id, message_id)
);

CREATE TABLE IF NOT EXISTS child_tasks (
    child_task_id       TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    parent_run_id       TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    parent_tool_call_id TEXT NOT NULL,
    title               TEXT NOT NULL,
    system_prompt_json  TEXT NOT NULL,
    agent_variant       TEXT NOT NULL CHECK (agent_variant IN ('plan', 'build')),
    status              TEXT NOT NULL CHECK (
                            status IN ('accepted', 'running', 'completed', 'failed',
                                       'cancelled', 'interrupted')),
    cancel_requested    INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
    body_generation     INTEGER NOT NULL CHECK (body_generation > 0),
    message_count       INTEGER NOT NULL CHECK (message_count >= 0),
    final_message_id    TEXT,
    error_code          TEXT,
    error_message       TEXT,
    created_at_ms       INTEGER NOT NULL,
    started_at_ms       INTEGER,
    finished_at_ms      INTEGER,
    UNIQUE (parent_run_id, parent_tool_call_id)
);

CREATE INDEX IF NOT EXISTS child_tasks_parent_order
    ON child_tasks(session_id, parent_run_id, created_at_ms, child_task_id);

CREATE TABLE IF NOT EXISTS child_pending_tool_exchanges (
    receipt_id          TEXT PRIMARY KEY,
    child_task_id       TEXT NOT NULL UNIQUE REFERENCES child_tasks(child_task_id) ON DELETE CASCADE,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    assistant_json      TEXT NOT NULL,
    results_json        TEXT,
    state               TEXT NOT NULL CHECK (state IN ('begun', 'ready')),
    created_at_ms       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS child_pending_tool_starts (
    receipt_id          TEXT NOT NULL REFERENCES child_pending_tool_exchanges(receipt_id) ON DELETE CASCADE,
    call_id             TEXT NOT NULL,
    started_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (receipt_id, call_id)
);

CREATE TABLE IF NOT EXISTS child_body_appends (
    operation_id        TEXT PRIMARY KEY,
    child_task_id       TEXT NOT NULL UNIQUE REFERENCES child_tasks(child_task_id) ON DELETE CASCADE,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    body_generation     INTEGER NOT NULL,
    base_byte_length    INTEGER NOT NULL CHECK (base_byte_length >= 0),
    kind                TEXT NOT NULL,
    payload             BLOB NOT NULL,
    message_count_delta INTEGER NOT NULL CHECK (message_count_delta > 0),
    created_at_ms       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_tool_exchanges (
    receipt_id          TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    run_id              TEXT NOT NULL UNIQUE REFERENCES runs(run_id) ON DELETE CASCADE,
    assistant_json      TEXT NOT NULL,
    results_json        TEXT,
    state               TEXT NOT NULL CHECK (state IN ('begun', 'ready')),
    created_at_ms       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_tool_starts (
    receipt_id          TEXT NOT NULL REFERENCES pending_tool_exchanges(receipt_id) ON DELETE CASCADE,
    call_id             TEXT NOT NULL,
    started_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (receipt_id, call_id)
);

CREATE TABLE IF NOT EXISTS body_appends (
    operation_id        TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL UNIQUE REFERENCES sessions(session_id) ON DELETE CASCADE,
    run_id              TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
    body_generation     INTEGER NOT NULL,
    base_byte_length    INTEGER NOT NULL CHECK (base_byte_length >= 0),
    kind                TEXT NOT NULL,
    payload             BLOB NOT NULL,
    message_count_delta INTEGER NOT NULL CHECK (message_count_delta > 0),
    created_at_ms       INTEGER NOT NULL
);
"#;

const REQUIRED_PROJECTIONS: &[&str] = &[
    "SELECT session_id, title, model_key, system_prompt_json, current_variant, approval_mode, lifecycle, body_generation, message_count, created_at_ms, updated_at_ms, archived_at_ms, is_pinned, title_origin FROM sessions LIMIT 0",
    "SELECT session_id, message_id, feedback, changed_at_ms FROM message_feedback LIMIT 0",
    "SELECT workspace_id, user_directory, agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms FROM workspaces LIMIT 0",
    "SELECT session_id, workspace_id, working_directory, attachment_directory, private_directory, created_at_ms FROM session_resources LIMIT 0",
    "SELECT blob_hash, size_bytes, relative_path, created_at_ms FROM attachment_blobs LIMIT 0",
    "SELECT attachment_id, session_id, blob_hash, original_name, agent_readable_path, state, created_at_ms FROM attachments LIMIT 0",
    "SELECT COALESCE(priority_order, queue_order), input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant FROM inputs LIMIT 0",
    "SELECT run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms FROM runs LIMIT 0",
    "SELECT run_id, message_id FROM run_message_refs LIMIT 0",
    "SELECT child_task_id, session_id, parent_run_id, parent_tool_call_id, title, system_prompt_json, agent_variant, status, cancel_requested, body_generation, message_count, final_message_id, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms FROM child_tasks LIMIT 0",
    "SELECT receipt_id, child_task_id, session_id, assistant_json, results_json, state, created_at_ms FROM child_pending_tool_exchanges LIMIT 0",
    "SELECT receipt_id, call_id, started_at_ms FROM child_pending_tool_starts LIMIT 0",
    "SELECT operation_id, child_task_id, session_id, body_generation, base_byte_length, kind, payload, message_count_delta, created_at_ms FROM child_body_appends LIMIT 0",
    "SELECT receipt_id, session_id, run_id, assistant_json, results_json, state, created_at_ms FROM pending_tool_exchanges LIMIT 0",
    "SELECT receipt_id, call_id, started_at_ms FROM pending_tool_starts LIMIT 0",
    "SELECT operation_id, session_id, run_id, body_generation, base_byte_length, kind, payload, message_count_delta, created_at_ms FROM body_appends LIMIT 0",
];

pub(super) fn initialize(connection: &mut Connection) -> StorageResult<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| {
            internal_error("runtime database schema could not be initialized", source)
        })?;
    transaction.execute_batch(SCHEMA).map_err(|source| {
        internal_error("runtime database schema could not be initialized", source)
    })?;
    ensure_column(
        &transaction,
        "sessions",
        "current_variant",
        "ALTER TABLE sessions ADD COLUMN current_variant TEXT NOT NULL DEFAULT 'build' CHECK (current_variant IN ('plan', 'build'))",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "priority_order",
        "ALTER TABLE inputs ADD COLUMN priority_order INTEGER",
    )?;
    ensure_column(
        &transaction,
        "sessions",
        "approval_mode",
        "ALTER TABLE sessions ADD COLUMN approval_mode TEXT NOT NULL DEFAULT 'ask' CHECK (approval_mode IN ('ask', 'auto'))",
    )?;
    ensure_column(
        &transaction,
        "sessions",
        "is_pinned",
        "ALTER TABLE sessions ADD COLUMN is_pinned INTEGER NOT NULL DEFAULT 0 CHECK (is_pinned IN (0, 1))",
    )?;
    ensure_column(
        &transaction,
        "sessions",
        "title_origin",
        "ALTER TABLE sessions ADD COLUMN title_origin TEXT NOT NULL DEFAULT 'generated' CHECK (title_origin IN ('generated', 'user'))",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "agent_variant",
        "ALTER TABLE inputs ADD COLUMN agent_variant TEXT NOT NULL DEFAULT 'build' CHECK (agent_variant IN ('plan', 'build'))",
    )?;
    ensure_column(
        &transaction,
        "runs",
        "approval_mode",
        "ALTER TABLE runs ADD COLUMN approval_mode TEXT NOT NULL DEFAULT 'ask' CHECK (approval_mode IN ('ask', 'auto'))",
    )?;
    transaction.commit().map_err(|source| {
        internal_error("runtime database schema could not be committed", source)
    })?;

    // CREATE IF NOT EXISTS 不会验证已存在表的列。逐个 prepare 当前读取投影，使不兼容的
    // 早期/手工 schema 在 Host 开放 socket 前明确失败，而不是运行中途才暴露。
    for projection in REQUIRED_PROJECTIONS {
        connection
            .prepare(projection)
            .map_err(|source| internal_error("runtime database schema is incompatible", source))?;
    }
    Ok(())
}

fn ensure_column(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    migration: &str,
) -> StorageResult<()> {
    let mut statement = transaction
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|source| {
            internal_error("runtime database schema could not be inspected", source)
        })?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| {
            internal_error("runtime database schema could not be inspected", source)
        })?;
    for existing in columns {
        if existing.map_err(|source| {
            internal_error("runtime database schema could not be inspected", source)
        })? == column
        {
            return Ok(());
        }
    }
    transaction
        .execute_batch(migration)
        .map_err(|source| internal_error("runtime database schema could not be migrated", source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_mode_columns_are_added_once_with_safe_defaults() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                    session_id TEXT PRIMARY KEY, title TEXT NOT NULL, model_key TEXT NOT NULL,
                    system_prompt_json TEXT NOT NULL, lifecycle TEXT NOT NULL,
                    body_generation INTEGER NOT NULL, message_count INTEGER NOT NULL,
                    created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
                    archived_at_ms INTEGER
                 );
                 CREATE TABLE inputs (
                    queue_order INTEGER PRIMARY KEY AUTOINCREMENT, input_id TEXT NOT NULL UNIQUE,
                    session_id TEXT NOT NULL, idempotency_key TEXT, user_message_id TEXT NOT NULL UNIQUE,
                    state TEXT NOT NULL, queued_message_json TEXT, accepted_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE runs (
                    run_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, input_id TEXT NOT NULL,
                    attempt INTEGER NOT NULL, status TEXT NOT NULL, cancel_requested INTEGER NOT NULL,
                    error_code TEXT, error_message TEXT, created_at_ms INTEGER NOT NULL,
                    started_at_ms INTEGER, finished_at_ms INTEGER
                 );",
            )
            .expect("legacy schema");

        initialize(&mut connection).expect("first migration");
        initialize(&mut connection).expect("idempotent second migration");
        connection
            .execute(
                "INSERT INTO sessions (session_id, title, model_key, system_prompt_json, lifecycle,
                    body_generation, message_count, created_at_ms, updated_at_ms)
                 VALUES ('s-legacy', 'Legacy', 'fixture', '{}', 'active', 1, 0, 1, 1)",
                [],
            )
            .expect("legacy-compatible session insert");
        connection
            .execute(
                "INSERT INTO inputs (input_id, session_id, user_message_id, state, accepted_at_ms)
                 VALUES ('i-legacy', 's-legacy', 'm-legacy', 'committed', 1)",
                [],
            )
            .expect("legacy-compatible input insert");
        connection
            .execute(
                "INSERT INTO runs (run_id, session_id, input_id, attempt, status, cancel_requested,
                    created_at_ms) VALUES ('r-legacy', 's-legacy', 'i-legacy', 1, 'completed', 0, 1)",
                [],
            )
            .expect("legacy-compatible run insert");

        let values: (String, String, String, String, Option<i64>) = connection
            .query_row(
                "SELECT sessions.current_variant, sessions.approval_mode,
                        inputs.agent_variant, runs.approval_mode, inputs.priority_order
                 FROM sessions JOIN inputs ON inputs.session_id = sessions.session_id
                 JOIN runs ON runs.input_id = inputs.input_id",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("migrated defaults");
        assert_eq!(
            values,
            (
                "build".into(),
                "ask".into(),
                "build".into(),
                "ask".into(),
                None,
            )
        );
    }
}
