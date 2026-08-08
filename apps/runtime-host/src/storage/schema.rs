//! SQLite schema 初始化与当前格式核验。

use rusqlite::{Connection, TransactionBehavior};

use super::{StorageResult, internal_error};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    session_id          TEXT PRIMARY KEY,
    title               TEXT NOT NULL,
    model_key           TEXT NOT NULL,
    system_prompt_json  TEXT NOT NULL,
    lifecycle           TEXT NOT NULL CHECK (lifecycle IN ('active', 'archived')),
    body_generation     INTEGER NOT NULL CHECK (body_generation > 0),
    message_count       INTEGER NOT NULL CHECK (message_count >= 0),
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL,
    archived_at_ms      INTEGER
);

CREATE TABLE IF NOT EXISTS inputs (
    queue_order         INTEGER PRIMARY KEY AUTOINCREMENT,
    input_id            TEXT NOT NULL UNIQUE,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    idempotency_key     TEXT,
    user_message_id     TEXT NOT NULL UNIQUE,
    state               TEXT NOT NULL CHECK (state IN ('queued', 'committed')),
    queued_message_json TEXT,
    accepted_at_ms      INTEGER NOT NULL,
    UNIQUE (session_id, idempotency_key)
);

CREATE TABLE IF NOT EXISTS runs (
    run_id              TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    input_id            TEXT NOT NULL REFERENCES inputs(input_id) ON DELETE CASCADE,
    attempt             INTEGER NOT NULL CHECK (attempt > 0),
    status              TEXT NOT NULL,
    cancel_requested    INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
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

CREATE TABLE IF NOT EXISTS pending_tool_exchanges (
    receipt_id          TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    run_id              TEXT NOT NULL UNIQUE REFERENCES runs(run_id) ON DELETE CASCADE,
    assistant_json      TEXT NOT NULL,
    results_json        TEXT,
    state               TEXT NOT NULL CHECK (state IN ('begun', 'ready')),
    created_at_ms       INTEGER NOT NULL
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
    "SELECT session_id, title, model_key, system_prompt_json, lifecycle, body_generation, message_count, created_at_ms, updated_at_ms, archived_at_ms FROM sessions LIMIT 0",
    "SELECT queue_order, input_id, session_id, idempotency_key, user_message_id, state, queued_message_json, accepted_at_ms FROM inputs LIMIT 0",
    "SELECT run_id, session_id, input_id, attempt, status, cancel_requested, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms FROM runs LIMIT 0",
    "SELECT run_id, message_id FROM run_message_refs LIMIT 0",
    "SELECT receipt_id, session_id, run_id, assistant_json, results_json, state, created_at_ms FROM pending_tool_exchanges LIMIT 0",
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
