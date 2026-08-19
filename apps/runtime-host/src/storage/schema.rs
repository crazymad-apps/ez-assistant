//! SQLite schema 初始化与当前格式核验。

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{StorageResult, internal_error};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    session_id          TEXT PRIMARY KEY,
    title               TEXT NOT NULL,
    model_key           TEXT NOT NULL,
    reasoning_effort    TEXT CHECK (reasoning_effort IS NULL OR reasoning_effort IN ('low','medium','high','xhigh','max')),
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

CREATE TABLE IF NOT EXISTS persona (
    singleton_key      INTEGER PRIMARY KEY CHECK (singleton_key = 1),
    enabled            INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    content            TEXT NOT NULL,
    revision           INTEGER NOT NULL CHECK (revision >= 0),
    updated_at_ms      INTEGER NOT NULL
);

INSERT OR IGNORE INTO persona (singleton_key, enabled, content, revision, updated_at_ms)
VALUES (1, 0, '', 0, 0);

CREATE TABLE IF NOT EXISTS memory_state (
    singleton_key              INTEGER PRIMARY KEY CHECK (singleton_key = 1),
    pinned_collection_revision INTEGER NOT NULL CHECK (pinned_collection_revision >= 0)
);

INSERT OR IGNORE INTO memory_state (singleton_key, pinned_collection_revision)
VALUES (1, 0);

CREATE TABLE IF NOT EXISTS pinned_memories (
    id                    TEXT PRIMARY KEY,
    category              TEXT NOT NULL,
    content               TEXT NOT NULL,
    attributes_json       TEXT NOT NULL,
    created_by_kind       TEXT NOT NULL CHECK (created_by_kind IN ('user', 'agent_tool')),
    created_by_session_id TEXT,
    revision              INTEGER NOT NULL CHECK (revision > 0),
    created_at_ms          INTEGER NOT NULL,
    updated_at_ms          INTEGER NOT NULL,
    CHECK ((created_by_kind = 'user' AND created_by_session_id IS NULL)
        OR (created_by_kind = 'agent_tool' AND created_by_session_id IS NOT NULL))
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
    media_type         TEXT,
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
    reasoning_effort    TEXT CHECK (reasoning_effort IS NULL OR reasoning_effort IN ('low','medium','high','xhigh','max')),
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

CREATE TABLE IF NOT EXISTS session_usage (
    session_id                    TEXT PRIMARY KEY REFERENCES sessions(session_id) ON DELETE CASCADE,
    request_count                 INTEGER NOT NULL DEFAULT 0 CHECK (request_count >= 0),
    input_tokens_sum              INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens_sum >= 0),
    output_tokens_sum             INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens_sum >= 0),
    total_tokens_sum              INTEGER NOT NULL DEFAULT 0 CHECK (total_tokens_sum >= 0),
    cached_input_tokens_sum       INTEGER NOT NULL DEFAULT 0 CHECK (cached_input_tokens_sum >= 0),
    cached_request_count          INTEGER NOT NULL DEFAULT 0 CHECK (cached_request_count >= 0),
    reasoning_tokens_sum          INTEGER NOT NULL DEFAULT 0 CHECK (reasoning_tokens_sum >= 0),
    reasoning_request_count       INTEGER NOT NULL DEFAULT 0 CHECK (reasoning_request_count >= 0),
    latest_input_tokens           INTEGER,
    latest_output_tokens          INTEGER,
    latest_total_tokens           INTEGER,
    latest_cached_input_tokens    INTEGER,
    latest_reasoning_tokens       INTEGER,
    backfilled                    INTEGER NOT NULL DEFAULT 0 CHECK (backfilled IN (0, 1)),
    updated_at_ms                 INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS model_request_records (
    session_id            TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    owner_kind            TEXT NOT NULL CHECK (owner_kind IN ('session', 'child_task')),
    owner_id              TEXT NOT NULL,
    request_id            TEXT NOT NULL,
    run_id                TEXT,
    request_kind          TEXT NOT NULL CHECK (
                              request_kind IN ('agent_turn', 'context_summary', 'legacy_compacted')),
    provider              TEXT,
    model_id              TEXT,
    input_tokens          INTEGER NOT NULL CHECK (input_tokens >= 0),
    output_tokens         INTEGER NOT NULL CHECK (output_tokens >= 0),
    total_tokens          INTEGER NOT NULL CHECK (total_tokens >= 0),
    cached_input_tokens   INTEGER CHECK (cached_input_tokens IS NULL OR cached_input_tokens >= 0),
    reasoning_tokens      INTEGER CHECK (reasoning_tokens IS NULL OR reasoning_tokens >= 0),
    completed_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (session_id, owner_kind, owner_id, request_id)
);

CREATE INDEX IF NOT EXISTS model_request_records_session_time
    ON model_request_records(session_id, completed_at_ms, request_id);

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

CREATE TABLE IF NOT EXISTS conversation_recall_documents (
    document_rowid      INTEGER PRIMARY KEY,
    document_id         TEXT NOT NULL UNIQUE,
    owner_kind         TEXT NOT NULL CHECK (owner_kind IN ('session', 'child_task')),
    owner_id           TEXT NOT NULL,
    session_id         TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    child_task_id      TEXT REFERENCES child_tasks(child_task_id) ON DELETE CASCADE,
    body_generation    INTEGER NOT NULL CHECK (body_generation > 0),
    message_id         TEXT NOT NULL,
    message_kind       TEXT NOT NULL CHECK (message_kind IN ('user', 'assistant')),
    message_ordinal    INTEGER NOT NULL CHECK (message_ordinal >= 0),
    created_at_ms      INTEGER NOT NULL,
    normalized_text    TEXT NOT NULL,
    content_hash       TEXT NOT NULL,
    UNIQUE (owner_kind, owner_id, body_generation, message_id)
);

CREATE INDEX IF NOT EXISTS conversation_recall_documents_owner
    ON conversation_recall_documents(owner_kind, owner_id, body_generation, message_ordinal);
CREATE INDEX IF NOT EXISTS conversation_recall_documents_session
    ON conversation_recall_documents(session_id, child_task_id, body_generation);

CREATE TABLE IF NOT EXISTS conversation_recall_heads (
    owner_kind             TEXT NOT NULL CHECK (owner_kind IN ('session', 'child_task')),
    owner_id               TEXT NOT NULL,
    session_id             TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    child_task_id          TEXT REFERENCES child_tasks(child_task_id) ON DELETE CASCADE,
    body_generation        INTEGER NOT NULL CHECK (body_generation > 0),
    indexed_message_count  INTEGER NOT NULL CHECK (indexed_message_count >= 0),
    state                  TEXT NOT NULL CHECK (state IN ('ready', 'dirty', 'rebuilding', 'unavailable')),
    updated_at_ms          INTEGER NOT NULL,
    PRIMARY KEY (owner_kind, owner_id)
);
"#;

const RECALL_FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS conversation_recall_fts USING fts5(
    normalized_text,
    content='conversation_recall_documents',
    content_rowid='document_rowid',
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS conversation_recall_documents_ai AFTER INSERT ON conversation_recall_documents BEGIN
    INSERT INTO conversation_recall_fts(rowid, normalized_text)
    VALUES (new.document_rowid, new.normalized_text);
END;
CREATE TRIGGER IF NOT EXISTS conversation_recall_documents_ad AFTER DELETE ON conversation_recall_documents BEGIN
    INSERT INTO conversation_recall_fts(conversation_recall_fts, rowid, normalized_text)
    VALUES ('delete', old.document_rowid, old.normalized_text);
END;
CREATE TRIGGER IF NOT EXISTS conversation_recall_documents_au AFTER UPDATE ON conversation_recall_documents BEGIN
    INSERT INTO conversation_recall_fts(conversation_recall_fts, rowid, normalized_text)
    VALUES ('delete', old.document_rowid, old.normalized_text);
    INSERT INTO conversation_recall_fts(rowid, normalized_text)
    VALUES (new.document_rowid, new.normalized_text);
END;
"#;

const REQUIRED_PROJECTIONS: &[&str] = &[
    "SELECT session_id, title, model_key, reasoning_effort, system_prompt_json, current_variant, approval_mode, lifecycle, body_generation, message_count, created_at_ms, updated_at_ms, archived_at_ms, is_pinned, title_origin FROM sessions LIMIT 0",
    "SELECT enabled, content, revision, updated_at_ms FROM persona WHERE singleton_key = 1",
    "SELECT pinned_collection_revision FROM memory_state WHERE singleton_key = 1",
    "SELECT id, category, content, attributes_json, created_by_kind, created_by_session_id, revision, created_at_ms, updated_at_ms FROM pinned_memories LIMIT 0",
    "SELECT session_id, message_id, feedback, changed_at_ms FROM message_feedback LIMIT 0",
    "SELECT workspace_id, user_directory, agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms FROM workspaces LIMIT 0",
    "SELECT session_id, workspace_id, working_directory, attachment_directory, private_directory, created_at_ms FROM session_resources LIMIT 0",
    "SELECT blob_hash, size_bytes, relative_path, media_type, created_at_ms FROM attachment_blobs LIMIT 0",
    "SELECT attachment_id, session_id, blob_hash, original_name, agent_readable_path, state, created_at_ms FROM attachments LIMIT 0",
    "SELECT COALESCE(priority_order, queue_order), input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms, agent_variant FROM inputs LIMIT 0",
    "SELECT run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, reasoning_effort, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms FROM runs LIMIT 0",
    "SELECT run_id, message_id FROM run_message_refs LIMIT 0",
    "SELECT session_id, request_count, input_tokens_sum, output_tokens_sum, total_tokens_sum, cached_input_tokens_sum, cached_request_count, reasoning_tokens_sum, reasoning_request_count, latest_input_tokens, latest_output_tokens, latest_total_tokens, latest_cached_input_tokens, latest_reasoning_tokens, backfilled, updated_at_ms FROM session_usage LIMIT 0",
    "SELECT session_id, owner_kind, owner_id, request_id, run_id, request_kind, provider, model_id, input_tokens, output_tokens, total_tokens, cached_input_tokens, reasoning_tokens, completed_at_ms FROM model_request_records LIMIT 0",
    "SELECT child_task_id, session_id, parent_run_id, parent_tool_call_id, title, system_prompt_json, agent_variant, status, cancel_requested, body_generation, message_count, final_message_id, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms FROM child_tasks LIMIT 0",
    "SELECT receipt_id, child_task_id, session_id, assistant_json, results_json, state, created_at_ms FROM child_pending_tool_exchanges LIMIT 0",
    "SELECT receipt_id, call_id, started_at_ms FROM child_pending_tool_starts LIMIT 0",
    "SELECT operation_id, child_task_id, session_id, body_generation, base_byte_length, kind, payload, message_count_delta, created_at_ms FROM child_body_appends LIMIT 0",
    "SELECT receipt_id, session_id, run_id, assistant_json, results_json, state, created_at_ms FROM pending_tool_exchanges LIMIT 0",
    "SELECT receipt_id, call_id, started_at_ms FROM pending_tool_starts LIMIT 0",
    "SELECT operation_id, session_id, run_id, body_generation, base_byte_length, kind, payload, message_count_delta, created_at_ms FROM body_appends LIMIT 0",
    "SELECT document_rowid, document_id, owner_kind, owner_id, session_id, child_task_id, body_generation, message_id, message_kind, message_ordinal, created_at_ms, normalized_text, content_hash FROM conversation_recall_documents LIMIT 0",
    "SELECT owner_kind, owner_id, session_id, child_task_id, body_generation, indexed_message_count, state, updated_at_ms FROM conversation_recall_heads LIMIT 0",
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
        "attachment_blobs",
        "media_type",
        "ALTER TABLE attachment_blobs ADD COLUMN media_type TEXT",
    )?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO session_usage (session_id, backfilled, updated_at_ms)
             SELECT session_id, 0, updated_at_ms FROM sessions",
            [],
        )
        .map_err(|source| {
            internal_error("session usage migration could not be initialized", source)
        })?;
    ensure_column(
        &transaction,
        "sessions",
        "reasoning_effort",
        "ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT CHECK (reasoning_effort IS NULL OR reasoning_effort IN ('low','medium','high','xhigh','max'));",
    )?;
    ensure_column(
        &transaction,
        "runs",
        "reasoning_effort",
        "ALTER TABLE runs ADD COLUMN reasoning_effort TEXT CHECK (reasoning_effort IS NULL OR reasoning_effort IN ('low','medium','high','xhigh','max'));",
    )?;
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
    transaction
        .execute(
            "UPDATE conversation_recall_heads
             SET state = 'dirty'
             WHERE state = 'rebuilding'",
            [],
        )
        .map_err(|source| {
            internal_error(
                "interrupted conversation recall rebuilds could not be recovered",
                source,
            )
        })?;
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

/// 探测并初始化本机 SQLite 的 trigram FTS5 能力；失败只关闭 Recall，不阻断 Runtime。
pub(super) fn initialize_recall_fts(connection: &mut Connection) -> bool {
    let existed = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'conversation_recall_fts'",
            [],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some();
    let Ok(transaction) = connection.transaction_with_behavior(TransactionBehavior::Immediate)
    else {
        return false;
    };
    if !existed
        && transaction
            .execute_batch(
                "DROP TRIGGER IF EXISTS conversation_recall_documents_ai;
                 DROP TRIGGER IF EXISTS conversation_recall_documents_ad;
                 DROP TRIGGER IF EXISTS conversation_recall_documents_au;
                 DELETE FROM conversation_recall_documents;
                 UPDATE conversation_recall_heads
                 SET indexed_message_count = 0, state = 'dirty';",
            )
            .is_err()
    {
        return false;
    }
    if transaction.execute_batch(RECALL_FTS_SCHEMA).is_err() {
        return false;
    }
    transaction.commit().is_ok()
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
        let persona: (i64, String, i64, i64) = connection
            .query_row(
                "SELECT enabled, content, revision, updated_at_ms
                 FROM persona WHERE singleton_key = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("default persona");
        assert_eq!(persona, (0, String::new(), 0, 0));
        assert_eq!(
            connection
                .query_row(
                    "SELECT pinned_collection_revision FROM memory_state
                     WHERE singleton_key = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("default pinned collection revision"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pinned_memories", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("empty pinned memories"),
            0
        );
    }
}
