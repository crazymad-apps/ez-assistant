//! SQLite schema 初始化与当前格式核验。

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{StorageResult, internal_error};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS devices (
    device_id       TEXT PRIMARY KEY,
    display_name    TEXT NOT NULL,
    public_key      BLOB NOT NULL UNIQUE,
    lifecycle       TEXT NOT NULL CHECK (lifecycle IN ('paired', 'revoked')),
    paired_at_ms    INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL,
    revoked_at_ms   INTEGER
);

CREATE TABLE IF NOT EXISTS sessions (
    session_id          TEXT PRIMARY KEY,
    title               TEXT NOT NULL,
    model_key           TEXT NOT NULL,
    reasoning_effort    TEXT CHECK (reasoning_effort IS NULL OR reasoning_effort IN ('low','medium','high','xhigh','max')),
    system_prompt_json  TEXT NOT NULL,
    skill_catalog_json  TEXT NOT NULL,
    current_variant     TEXT NOT NULL DEFAULT 'build'
                            CHECK (current_variant IN ('plan', 'build')),
    approval_mode       TEXT NOT NULL DEFAULT 'ask'
                            CHECK (approval_mode IN ('ask', 'auto')),
    role                TEXT NOT NULL DEFAULT 'standard'
                            CHECK (role IN ('standard', 'controller')),
    proxy_controller_session_id TEXT REFERENCES sessions(session_id) ON DELETE SET NULL,
    proxy_changed_at_ms INTEGER,
    pc_output_device_id TEXT REFERENCES devices(device_id) ON DELETE SET NULL,
    lifecycle           TEXT NOT NULL CHECK (lifecycle IN ('active', 'archived')),
    body_generation     INTEGER NOT NULL CHECK (body_generation > 0),
    message_count       INTEGER NOT NULL CHECK (message_count >= 0),
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL,
    archived_at_ms      INTEGER,
    is_pinned           INTEGER NOT NULL DEFAULT 0 CHECK (is_pinned IN (0, 1)),
    title_origin        TEXT NOT NULL DEFAULT 'generated'
                            CHECK (title_origin IN ('generated', 'user')),
    materialization_key TEXT,
    automatic_title_pending INTEGER NOT NULL DEFAULT 0
                            CHECK (automatic_title_pending IN (0, 1))
);

CREATE TABLE IF NOT EXISTS session_history_operations (
    operation_id      TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    kind              TEXT NOT NULL CHECK (kind IN ('clear', 'compact')),
    state             TEXT NOT NULL CHECK (state IN (
                          'preparing', 'no_op', 'cleanup_pending', 'completed',
                          'cancelled', 'interrupted')),
    source_generation INTEGER NOT NULL CHECK (source_generation > 0),
    result_generation INTEGER CHECK (result_generation IS NULL OR result_generation > 0),
    compacted_message_count INTEGER CHECK (
        compacted_message_count IS NULL OR compacted_message_count >= 0
    ),
    retained_message_count INTEGER CHECK (
        retained_message_count IS NULL OR retained_message_count >= 0
    ),
    created_at_ms     INTEGER NOT NULL,
    finished_at_ms    INTEGER
);

CREATE INDEX IF NOT EXISTS session_history_operations_session
    ON session_history_operations(session_id, created_at_ms, operation_id);

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

CREATE TABLE IF NOT EXISTS skill_name_states (
    name          TEXT PRIMARY KEY,
    enabled       INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= 0)
);

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

CREATE TABLE IF NOT EXISTS session_work_plans (
    session_id          TEXT PRIMARY KEY REFERENCES sessions(session_id) ON DELETE CASCADE,
    revision            INTEGER NOT NULL CHECK (revision > 0),
    objective           TEXT NOT NULL,
    items_json          TEXT NOT NULL,
    last_operation_id   TEXT NOT NULL,
    updated_at_ms       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS work_plan_completion_receipts (
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    operation_id        TEXT NOT NULL,
    revision            INTEGER NOT NULL CHECK (revision > 0),
    objective           TEXT NOT NULL,
    items_json          TEXT NOT NULL,
    updated_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (session_id, operation_id)
);

CREATE TABLE IF NOT EXISTS session_goals (
    goal_id                    TEXT PRIMARY KEY,
    session_id                 TEXT NOT NULL UNIQUE REFERENCES sessions(session_id) ON DELETE CASCADE,
    objective_message_id       TEXT NOT NULL,
    objective_payload_json     TEXT NOT NULL,
    objective_hash             TEXT NOT NULL,
    mcp_server_key             TEXT,
    state                      TEXT NOT NULL CHECK (state IN ('running', 'paused', 'completed')),
    pause_reason_json          TEXT,
    generation                 INTEGER NOT NULL CHECK (generation > 0),
    turn                       INTEGER NOT NULL CHECK (turn > 0),
    max_runs                   INTEGER NOT NULL CHECK (max_runs > 0),
    max_total_tokens           INTEGER NOT NULL CHECK (max_total_tokens > 0),
    max_consecutive_failures   INTEGER NOT NULL CHECK (max_consecutive_failures > 0),
    used_runs                  INTEGER NOT NULL CHECK (used_runs >= 0),
    used_total_tokens          INTEGER NOT NULL CHECK (used_total_tokens >= 0),
    usage_complete             INTEGER NOT NULL CHECK (usage_complete IN (0, 1)),
    consecutive_failures       INTEGER NOT NULL CHECK (consecutive_failures >= 0),
    created_at_ms              INTEGER NOT NULL,
    updated_at_ms              INTEGER NOT NULL,
    completed_at_ms            INTEGER,
    CHECK ((state = 'paused' AND pause_reason_json IS NOT NULL)
        OR (state IN ('running', 'completed') AND pause_reason_json IS NULL)),
    CHECK ((state = 'completed' AND completed_at_ms IS NOT NULL)
        OR (state != 'completed' AND completed_at_ms IS NULL))
);

CREATE TABLE IF NOT EXISTS workspaces (
    workspace_id       TEXT PRIMARY KEY,
    label              TEXT NOT NULL,
    user_directory     TEXT NOT NULL UNIQUE,
    additional_directories_json TEXT NOT NULL DEFAULT '[]',
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
    additional_workspace_directories_json TEXT NOT NULL DEFAULT '[]',
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
    input_kind          TEXT NOT NULL DEFAULT 'message'
                            CHECK (input_kind IN ('message', 'command')),
    command_json        TEXT,
    command_result_json TEXT,
    queued_message_json TEXT,
    accepted_at_ms      INTEGER NOT NULL,
    agent_variant       TEXT NOT NULL DEFAULT 'build'
                            CHECK (agent_variant IN ('plan', 'build')),
    origin              TEXT NOT NULL DEFAULT 'user'
                            CHECK (origin IN ('user', 'runtime')),
    goal_id             TEXT,
    goal_generation     INTEGER CHECK (goal_generation IS NULL OR goal_generation > 0),
    goal_turn           INTEGER CHECK (goal_turn IS NULL OR goal_turn > 0),
    goal_reply_route_json TEXT,
    skill_activation_json TEXT,
    cross_session_json TEXT,
    channel_source_json TEXT,
    CHECK ((goal_id IS NULL AND goal_generation IS NULL AND goal_turn IS NULL)
        OR (goal_id IS NOT NULL AND goal_generation IS NOT NULL AND goal_turn IS NOT NULL)),
    CHECK (
        (input_kind = 'message' AND command_json IS NULL AND command_result_json IS NULL)
        OR
        (input_kind = 'command'
            AND queued_message_json IS NULL
            AND command_json IS NOT NULL
            AND origin = 'runtime'
            AND goal_id IS NULL
            AND goal_generation IS NULL
            AND goal_turn IS NULL
            AND goal_reply_route_json IS NULL
            AND skill_activation_json IS NULL
            AND cross_session_json IS NULL
            AND channel_source_json IS NULL
            AND ((state = 'queued' AND command_result_json IS NULL)
                OR (state = 'committed' AND command_result_json IS NOT NULL)))
    ),
    UNIQUE (session_id, idempotency_key)
);

CREATE TABLE IF NOT EXISTS mcp_input_selections (
    selection_id       TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    input_id           TEXT REFERENCES inputs(input_id) ON DELETE CASCADE,
    message_id         TEXT NOT NULL,
    server_key         TEXT NOT NULL,
    display_name       TEXT NOT NULL,
    created_at_ms      INTEGER NOT NULL,
    UNIQUE (session_id, message_id)
);

CREATE INDEX IF NOT EXISTS mcp_input_selections_session_order
    ON mcp_input_selections(session_id, created_at_ms, selection_id);

CREATE TABLE IF NOT EXISTS skill_activations (
    activation_id       TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    owner_kind          TEXT NOT NULL CHECK (owner_kind IN ('session', 'child_task')),
    owner_id            TEXT NOT NULL,
    run_id              TEXT REFERENCES runs(run_id) ON DELETE CASCADE,
    input_id            TEXT REFERENCES inputs(input_id) ON DELETE CASCADE,
    message_id          TEXT NOT NULL,
    name                TEXT NOT NULL,
    catalog_revision    TEXT NOT NULL,
    definition_digest   TEXT NOT NULL,
    trigger             TEXT NOT NULL CHECK (trigger IN ('user', 'model')),
    created_at_ms       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS skill_activations_session_order
    ON skill_activations(session_id, created_at_ms, activation_id);

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
    step                INTEGER CHECK (step IS NULL OR step > 0),
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
    auxiliary_request_count       INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_request_count >= 0),
    auxiliary_input_tokens_sum    INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_input_tokens_sum >= 0),
    auxiliary_output_tokens_sum   INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_output_tokens_sum >= 0),
    auxiliary_total_tokens_sum    INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_total_tokens_sum >= 0),
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
    step                INTEGER CHECK (step IS NULL OR step > 0),
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
    message_step        INTEGER CHECK (message_step IS NULL OR message_step > 0),
    created_at_ms       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pending_tool_exchanges (
    receipt_id          TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    run_id              TEXT NOT NULL UNIQUE REFERENCES runs(run_id) ON DELETE CASCADE,
    step                INTEGER CHECK (step IS NULL OR step > 0),
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
    message_step        INTEGER CHECK (message_step IS NULL OR message_step > 0),
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
    "SELECT session_id, title, model_key, reasoning_effort, system_prompt_json, skill_catalog_json, current_variant, approval_mode, lifecycle, body_generation, message_count, created_at_ms, updated_at_ms, archived_at_ms, is_pinned, title_origin, pc_output_device_id, materialization_key, automatic_title_pending FROM sessions LIMIT 0",
    "SELECT device_id, display_name, public_key, lifecycle, paired_at_ms, updated_at_ms, revoked_at_ms FROM devices LIMIT 0",
    "SELECT operation_id, session_id, kind, state, source_generation, result_generation, compacted_message_count, retained_message_count, created_at_ms, finished_at_ms FROM session_history_operations LIMIT 0",
    "SELECT enabled, content, revision, updated_at_ms FROM persona WHERE singleton_key = 1",
    "SELECT pinned_collection_revision FROM memory_state WHERE singleton_key = 1",
    "SELECT name, enabled, updated_at_ms FROM skill_name_states LIMIT 0",
    "SELECT id, category, content, attributes_json, created_by_kind, created_by_session_id, revision, created_at_ms, updated_at_ms FROM pinned_memories LIMIT 0",
    "SELECT session_id, message_id, feedback, changed_at_ms FROM message_feedback LIMIT 0",
    "SELECT session_id, revision, objective, items_json, last_operation_id, updated_at_ms FROM session_work_plans LIMIT 0",
    "SELECT session_id, operation_id, revision, objective, items_json, updated_at_ms FROM work_plan_completion_receipts LIMIT 0",
    "SELECT goal_id, session_id, objective_message_id, objective_payload_json, objective_hash, mcp_server_key, state, pause_reason_json, generation, turn, max_runs, max_total_tokens, max_consecutive_failures, used_runs, used_total_tokens, usage_complete, consecutive_failures, created_at_ms, updated_at_ms, completed_at_ms FROM session_goals LIMIT 0",
    "SELECT workspace_id, label, user_directory, additional_directories_json, agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms FROM workspaces LIMIT 0",
    "SELECT session_id, workspace_id, working_directory, additional_workspace_directories_json, attachment_directory, private_directory, created_at_ms FROM session_resources LIMIT 0",
    "SELECT blob_hash, size_bytes, relative_path, media_type, created_at_ms FROM attachment_blobs LIMIT 0",
    "SELECT attachment_id, session_id, blob_hash, original_name, agent_readable_path, state, created_at_ms FROM attachments LIMIT 0",
    "SELECT COALESCE(priority_order, queue_order), input_id, session_id, idempotency_key, user_message_id, state, input_kind, command_json, command_result_json, queued_message_json, accepted_at_ms, agent_variant, origin, goal_id, goal_generation, goal_turn, goal_reply_route_json, skill_activation_json, cross_session_json, channel_source_json FROM inputs LIMIT 0",
    "SELECT selection_id, session_id, input_id, message_id, server_key, display_name, created_at_ms FROM mcp_input_selections LIMIT 0",
    "SELECT activation_id, session_id, owner_kind, owner_id, run_id, input_id, message_id, name, catalog_revision, definition_digest, trigger, created_at_ms FROM skill_activations LIMIT 0",
    "SELECT run_id, session_id, input_id, attempt, status, cancel_requested, approval_mode, reasoning_effort, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms FROM runs LIMIT 0",
    "SELECT run_id, message_id, step FROM run_message_refs LIMIT 0",
    "SELECT session_id, request_count, input_tokens_sum, output_tokens_sum, total_tokens_sum, cached_input_tokens_sum, cached_request_count, reasoning_tokens_sum, reasoning_request_count, auxiliary_request_count, auxiliary_input_tokens_sum, auxiliary_output_tokens_sum, auxiliary_total_tokens_sum, latest_input_tokens, latest_output_tokens, latest_total_tokens, latest_cached_input_tokens, latest_reasoning_tokens, backfilled, updated_at_ms FROM session_usage LIMIT 0",
    "SELECT session_id, owner_kind, owner_id, request_id, run_id, request_kind, provider, model_id, input_tokens, output_tokens, total_tokens, cached_input_tokens, reasoning_tokens, completed_at_ms FROM model_request_records LIMIT 0",
    "SELECT child_task_id, session_id, parent_run_id, parent_tool_call_id, title, system_prompt_json, agent_variant, status, cancel_requested, body_generation, message_count, final_message_id, error_code, error_message, created_at_ms, started_at_ms, finished_at_ms FROM child_tasks LIMIT 0",
    "SELECT receipt_id, child_task_id, session_id, step, assistant_json, results_json, state, created_at_ms FROM child_pending_tool_exchanges LIMIT 0",
    "SELECT receipt_id, call_id, started_at_ms FROM child_pending_tool_starts LIMIT 0",
    "SELECT operation_id, child_task_id, session_id, body_generation, base_byte_length, kind, payload, message_count_delta, message_step, created_at_ms FROM child_body_appends LIMIT 0",
    "SELECT receipt_id, session_id, run_id, step, assistant_json, results_json, state, created_at_ms FROM pending_tool_exchanges LIMIT 0",
    "SELECT receipt_id, call_id, started_at_ms FROM pending_tool_starts LIMIT 0",
    "SELECT operation_id, session_id, run_id, body_generation, base_byte_length, kind, payload, message_count_delta, message_step, created_at_ms FROM body_appends LIMIT 0",
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
    // v0.18.0 早期开发构建可能留下终态控制记录；正文与 Run 历史不在这些表中。
    transaction
        .execute_batch(
            "DELETE FROM session_work_plans
             WHERE NOT EXISTS (
                   SELECT 1 FROM json_each(session_work_plans.items_json)
                   WHERE json_extract(value, '$.status') != 'completed'
               );
             DELETE FROM session_goals WHERE state = 'completed';",
        )
        .map_err(|source| {
            internal_error("completed control state could not be migrated", source)
        })?;
    ensure_column(
        &transaction,
        "session_history_operations",
        "compacted_message_count",
        "ALTER TABLE session_history_operations ADD COLUMN compacted_message_count INTEGER CHECK (compacted_message_count IS NULL OR compacted_message_count >= 0)",
    )?;
    ensure_column(
        &transaction,
        "session_history_operations",
        "retained_message_count",
        "ALTER TABLE session_history_operations ADD COLUMN retained_message_count INTEGER CHECK (retained_message_count IS NULL OR retained_message_count >= 0)",
    )?;
    ensure_column(
        &transaction,
        "attachment_blobs",
        "media_type",
        "ALTER TABLE attachment_blobs ADD COLUMN media_type TEXT",
    )?;
    ensure_column(
        &transaction,
        "run_message_refs",
        "step",
        "ALTER TABLE run_message_refs ADD COLUMN step INTEGER CHECK (step IS NULL OR step > 0)",
    )?;
    ensure_column(
        &transaction,
        "pending_tool_exchanges",
        "step",
        "ALTER TABLE pending_tool_exchanges ADD COLUMN step INTEGER CHECK (step IS NULL OR step > 0)",
    )?;
    ensure_column(
        &transaction,
        "child_pending_tool_exchanges",
        "step",
        "ALTER TABLE child_pending_tool_exchanges ADD COLUMN step INTEGER CHECK (step IS NULL OR step > 0)",
    )?;
    ensure_column(
        &transaction,
        "body_appends",
        "message_step",
        "ALTER TABLE body_appends ADD COLUMN message_step INTEGER CHECK (message_step IS NULL OR message_step > 0)",
    )?;
    ensure_column(
        &transaction,
        "child_body_appends",
        "message_step",
        "ALTER TABLE child_body_appends ADD COLUMN message_step INTEGER CHECK (message_step IS NULL OR message_step > 0)",
    )?;
    ensure_column(
        &transaction,
        "workspaces",
        "label",
        "ALTER TABLE workspaces ADD COLUMN label TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        &transaction,
        "workspaces",
        "additional_directories_json",
        "ALTER TABLE workspaces ADD COLUMN additional_directories_json TEXT NOT NULL DEFAULT '[]'",
    )?;
    backfill_workspace_labels(&transaction)?;
    ensure_column(
        &transaction,
        "session_resources",
        "additional_workspace_directories_json",
        "ALTER TABLE session_resources ADD COLUMN additional_workspace_directories_json TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        &transaction,
        "sessions",
        "materialization_key",
        "ALTER TABLE sessions ADD COLUMN materialization_key TEXT",
    )?;
    ensure_column(
        &transaction,
        "sessions",
        "automatic_title_pending",
        "ALTER TABLE sessions ADD COLUMN automatic_title_pending INTEGER NOT NULL DEFAULT 0 CHECK (automatic_title_pending IN (0, 1))",
    )?;
    transaction
        .execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS sessions_materialization_key
             ON sessions(materialization_key) WHERE materialization_key IS NOT NULL",
            [],
        )
        .map_err(|source| {
            internal_error(
                "session materialization identity could not be initialized",
                source,
            )
        })?;
    for (column, migration) in [
        (
            "auxiliary_request_count",
            "ALTER TABLE session_usage ADD COLUMN auxiliary_request_count INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_request_count >= 0)",
        ),
        (
            "auxiliary_input_tokens_sum",
            "ALTER TABLE session_usage ADD COLUMN auxiliary_input_tokens_sum INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_input_tokens_sum >= 0)",
        ),
        (
            "auxiliary_output_tokens_sum",
            "ALTER TABLE session_usage ADD COLUMN auxiliary_output_tokens_sum INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_output_tokens_sum >= 0)",
        ),
        (
            "auxiliary_total_tokens_sum",
            "ALTER TABLE session_usage ADD COLUMN auxiliary_total_tokens_sum INTEGER NOT NULL DEFAULT 0 CHECK (auxiliary_total_tokens_sum >= 0)",
        ),
    ] {
        ensure_column(&transaction, "session_usage", column, migration)?;
    }
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
        "sessions",
        "skill_catalog_json",
        "ALTER TABLE sessions ADD COLUMN skill_catalog_json TEXT NOT NULL DEFAULT '{\"schema_version\":1,\"revision\":\"sha256-v1:92279a522f56969beaee47d8c8e03a5b73496e4e40dbb3e1810d15e2ff80e036\",\"status\":\"legacy_unavailable\",\"definitions\":[],\"diagnostics\":[]}'",
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
        "sessions",
        "role",
        "ALTER TABLE sessions ADD COLUMN role TEXT NOT NULL DEFAULT 'standard' CHECK (role IN ('standard', 'controller'))",
    )?;
    ensure_column(
        &transaction,
        "sessions",
        "proxy_controller_session_id",
        "ALTER TABLE sessions ADD COLUMN proxy_controller_session_id TEXT REFERENCES sessions(session_id) ON DELETE SET NULL",
    )?;
    ensure_column(
        &transaction,
        "sessions",
        "proxy_changed_at_ms",
        "ALTER TABLE sessions ADD COLUMN proxy_changed_at_ms INTEGER",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "agent_variant",
        "ALTER TABLE inputs ADD COLUMN agent_variant TEXT NOT NULL DEFAULT 'build' CHECK (agent_variant IN ('plan', 'build'))",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "origin",
        "ALTER TABLE inputs ADD COLUMN origin TEXT NOT NULL DEFAULT 'user' CHECK (origin IN ('user', 'runtime'))",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "goal_id",
        "ALTER TABLE inputs ADD COLUMN goal_id TEXT",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "goal_generation",
        "ALTER TABLE inputs ADD COLUMN goal_generation INTEGER CHECK (goal_generation IS NULL OR goal_generation > 0)",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "goal_turn",
        "ALTER TABLE inputs ADD COLUMN goal_turn INTEGER CHECK (goal_turn IS NULL OR goal_turn > 0)",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "goal_reply_route_json",
        "ALTER TABLE inputs ADD COLUMN goal_reply_route_json TEXT",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "skill_activation_json",
        "ALTER TABLE inputs ADD COLUMN skill_activation_json TEXT",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "cross_session_json",
        "ALTER TABLE inputs ADD COLUMN cross_session_json TEXT",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "channel_source_json",
        "ALTER TABLE inputs ADD COLUMN channel_source_json TEXT",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "input_kind",
        "ALTER TABLE inputs ADD COLUMN input_kind TEXT NOT NULL DEFAULT 'message' CHECK (input_kind IN ('message', 'command'))",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "command_json",
        "ALTER TABLE inputs ADD COLUMN command_json TEXT",
    )?;
    ensure_column(
        &transaction,
        "inputs",
        "command_result_json",
        "ALTER TABLE inputs ADD COLUMN command_result_json TEXT",
    )?;
    ensure_column(
        &transaction,
        "session_goals",
        "mcp_server_key",
        "ALTER TABLE session_goals ADD COLUMN mcp_server_key TEXT",
    )?;
    validate_queue_payloads(&transaction)?;
    ensure_column(
        &transaction,
        "sessions",
        "pc_output_device_id",
        "ALTER TABLE sessions ADD COLUMN pc_output_device_id TEXT REFERENCES devices(device_id) ON DELETE SET NULL",
    )?;
    transaction
        .execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS inputs_goal_turn
             ON inputs(goal_id, goal_generation, goal_turn)
             WHERE goal_id IS NOT NULL",
            [],
        )
        .map_err(|source| {
            internal_error("goal input uniqueness could not be initialized", source)
        })?;
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

/// ALTER TABLE 无法为既有 `inputs` 补整表 CHECK，因此每次启动都在同一迁移事务内
/// fail closed。M0 尚不消费 Command；这项核验只保证旧 message 不被误解释为 Command，
/// 也阻止半写入的 Command 在后续恢复阶段伪装成普通 Input/Run。
fn validate_queue_payloads(transaction: &rusqlite::Transaction<'_>) -> StorageResult<()> {
    let invalid = transaction
        .query_row(
            "SELECT 1 FROM inputs
             WHERE NOT (
                 (input_kind = 'message'
                    AND command_json IS NULL
                    AND command_result_json IS NULL)
                 OR
                 (input_kind = 'command'
                    AND queued_message_json IS NULL
                    AND command_json IS NOT NULL
                    AND origin = 'runtime'
                    AND goal_id IS NULL
                    AND goal_generation IS NULL
                    AND goal_turn IS NULL
                    AND goal_reply_route_json IS NULL
                    AND skill_activation_json IS NULL
                    AND cross_session_json IS NULL
                    AND channel_source_json IS NULL
                    AND ((state = 'queued' AND command_result_json IS NULL)
                        OR (state = 'committed' AND command_result_json IS NOT NULL)))
             )
             LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|source| {
            internal_error("runtime queue payloads could not be validated", source)
        })?;
    if invalid.is_some() {
        return Err(internal_error(
            "runtime queue payloads are incompatible",
            rusqlite::Error::InvalidQuery,
        ));
    }
    let invalid_selection = transaction
        .query_row(
            "SELECT 1 FROM mcp_input_selections AS selections
             WHERE selections.input_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM inputs
                   WHERE inputs.input_id = selections.input_id
                     AND inputs.session_id = selections.session_id
                     AND inputs.user_message_id = selections.message_id
                     AND inputs.input_kind = 'message'
               )
             LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|source| {
            internal_error(
                "MCP input selection relations could not be validated",
                source,
            )
        })?;
    if invalid_selection.is_some() {
        return Err(internal_error(
            "MCP input selection relations are incompatible",
            rusqlite::Error::InvalidQuery,
        ));
    }
    Ok(())
}

/// 旧 Workspace 没有独立标签；迁移只从已经持久化的主目录确定性补齐，不访问目录内容。
fn backfill_workspace_labels(transaction: &rusqlite::Transaction<'_>) -> StorageResult<()> {
    let rows = {
        let mut statement = transaction
            .prepare(
                "SELECT workspace_id, user_directory FROM workspaces
                 WHERE label = '' ORDER BY workspace_id",
            )
            .map_err(|source| internal_error("workspace labels could not be inspected", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| internal_error("workspace labels could not be inspected", source))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|source| internal_error("workspace labels could not be inspected", source))?
    };
    for (workspace_id, directory) in rows {
        let label = Path::new(&directory)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("工作空间");
        transaction
            .execute(
                "UPDATE workspaces SET label = ?1 WHERE workspace_id = ?2 AND label = ''",
                (label, workspace_id),
            )
            .map_err(|source| internal_error("workspace labels could not be migrated", source))?;
    }
    Ok(())
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
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'skill_name_states'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("skill name state table"),
            1
        );
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
        assert_eq!(
            connection
                .query_row(
                    "SELECT materialization_key, automatic_title_pending FROM sessions
                     WHERE session_id = 's-legacy'",
                    [],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
                )
                .expect("session v0.22 defaults"),
            (None, 0)
        );
        connection
            .execute(
                "INSERT INTO sessions (session_id, title, model_key, system_prompt_json, lifecycle,
                    body_generation, message_count, created_at_ms, updated_at_ms)
                 VALUES ('s-null-key', 'Null key', 'fixture', '{}', 'active', 1, 0, 2, 2)",
                [],
            )
            .expect("multiple null materialization keys remain valid");
        connection
            .execute(
                "UPDATE sessions SET materialization_key = 'materialize-1'
                 WHERE session_id = 's-legacy'",
                [],
            )
            .expect("first materialization key");
        assert!(
            connection
                .execute(
                    "UPDATE sessions SET materialization_key = 'materialize-1'
                     WHERE session_id = 's-null-key'",
                    [],
                )
                .is_err(),
            "non-null materialization keys must be unique"
        );
        connection
            .execute(
                "INSERT INTO session_resources (
                    session_id, workspace_id, working_directory, attachment_directory,
                    private_directory, created_at_ms
                 ) VALUES ('s-legacy', NULL, '/tmp/private', '/tmp/attachments',
                           '/tmp/private', 1)",
                [],
            )
            .expect("legacy-compatible resources insert");
        assert_eq!(
            connection
                .query_row(
                    "SELECT additional_workspace_directories_json FROM session_resources
                     WHERE session_id = 's-legacy'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .expect("session resource v0.22 default"),
            "[]"
        );
        connection
            .execute(
                "INSERT INTO session_usage (session_id) VALUES ('s-legacy')",
                [],
            )
            .expect("legacy-compatible usage insert");
        assert_eq!(
            connection
                .query_row(
                    "SELECT auxiliary_request_count, auxiliary_input_tokens_sum,
                            auxiliary_output_tokens_sum, auxiliary_total_tokens_sum
                     FROM session_usage WHERE session_id = 's-legacy'",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .expect("auxiliary usage defaults"),
            (0, 0, 0, 0)
        );

        let values = connection
            .query_row(
                "SELECT sessions.current_variant, sessions.approval_mode,
                        inputs.agent_variant, runs.approval_mode, inputs.priority_order,
                        sessions.role, sessions.proxy_controller_session_id,
                        sessions.proxy_changed_at_ms, inputs.input_kind,
                        inputs.command_json, inputs.command_result_json
                 FROM sessions JOIN inputs ON inputs.session_id = sessions.session_id
                 JOIN runs ON runs.input_id = inputs.input_id",
                [],
                |row| {
                    Ok((
                        (
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ),
                        (
                            row.get::<_, Option<i64>>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, Option<i64>>(7)?,
                            row.get::<_, String>(8)?,
                            row.get::<_, Option<String>>(9)?,
                            row.get::<_, Option<String>>(10)?,
                        ),
                    ))
                },
            )
            .expect("migrated defaults");
        assert_eq!(
            values,
            (
                ("build".into(), "ask".into(), "build".into(), "ask".into(),),
                (
                    None,
                    "standard".into(),
                    None,
                    None,
                    "message".into(),
                    None,
                    None,
                ),
            )
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'mcp_input_selections'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("MCP selection table"),
            1
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
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM session_work_plans", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("legacy sessions start without work plans"),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM session_goals", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("legacy sessions start without goals"),
            0
        );
    }

    #[test]
    fn legacy_workspace_label_is_derived_once_and_directories_default_empty() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");
        connection
            .execute_batch(
                "CREATE TABLE workspaces (
                    workspace_id TEXT PRIMARY KEY,
                    user_directory TEXT NOT NULL UNIQUE,
                    agent_directory TEXT NOT NULL UNIQUE,
                    lifecycle TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    removed_at_ms INTEGER
                 );
                 INSERT INTO workspaces (
                    workspace_id, user_directory, agent_directory, lifecycle,
                    created_at_ms, updated_at_ms
                 ) VALUES (
                    'w-legacy', '/tmp/legacy-project', '/tmp/runtime/w-legacy',
                    'active', 1, 1
                 );",
            )
            .expect("legacy workspace schema");

        initialize(&mut connection).expect("first migration");
        initialize(&mut connection).expect("idempotent second migration");
        assert_eq!(
            connection
                .query_row(
                    "SELECT label, additional_directories_json FROM workspaces
                     WHERE workspace_id = 'w-legacy'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .expect("migrated workspace defaults"),
            ("legacy-project".to_owned(), "[]".to_owned())
        );
    }

    #[test]
    fn current_schema_enforces_message_and_command_payload_exclusion() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");
        initialize(&mut connection).expect("initialize schema");
        connection
            .execute(
                "INSERT INTO sessions (
                    session_id, title, model_key, system_prompt_json, skill_catalog_json,
                    lifecycle, body_generation, message_count, created_at_ms, updated_at_ms
                 ) VALUES ('s-mcp', 'MCP', 'fixture', '{}', '{}', 'active', 1, 0, 1, 1)",
                [],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO inputs (
                    input_id, session_id, user_message_id, state, input_kind, command_json,
                    accepted_at_ms, origin
                 ) VALUES (
                    'command-queued', 's-mcp', 'message-command-queued', 'queued', 'command',
                    '{\"type\":\"mcp_refresh\",\"payload\":{}}', 2, 'runtime'
                 )",
                [],
            )
            .expect("valid queued command");
        connection
            .execute(
                "INSERT INTO inputs (
                    input_id, session_id, user_message_id, state, input_kind, command_json,
                    command_result_json, accepted_at_ms, origin
                 ) VALUES (
                    'command-committed', 's-mcp', 'message-command-committed', 'committed',
                    'command', '{\"type\":\"mcp_refresh\",\"payload\":{}}',
                    '{\"outcome\":\"success\",\"servers\":[]}', 3, 'runtime'
                 )",
                [],
            )
            .expect("valid committed command");
        assert!(
            connection
                .execute(
                    "INSERT INTO inputs (
                        input_id, session_id, user_message_id, state, input_kind, command_json,
                        accepted_at_ms, origin
                     ) VALUES (
                        'bad-message', 's-mcp', 'message-bad', 'queued', 'message', '{}', 4,
                        'user'
                     )",
                    [],
                )
                .is_err(),
            "message rows cannot contain command payloads"
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO inputs (
                        input_id, session_id, user_message_id, state, input_kind, command_json,
                        queued_message_json, accepted_at_ms, origin
                     ) VALUES (
                        'bad-command', 's-mcp', 'message-bad-command', 'queued', 'command', '{}',
                        '{}', 5, 'runtime'
                     )",
                    [],
                )
                .is_err(),
            "command rows cannot contain queued messages"
        );
    }

    #[test]
    fn startup_rejects_a_half_written_command_even_without_table_check() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");
        initialize(&mut connection).expect("initialize schema");
        connection
            .execute_batch(
                "PRAGMA ignore_check_constraints = ON;
                 INSERT INTO sessions (
                    session_id, title, model_key, system_prompt_json, skill_catalog_json,
                    lifecycle, body_generation, message_count, created_at_ms, updated_at_ms
                 ) VALUES ('s-invalid', 'Invalid', 'fixture', '{}', '{}', 'active', 1, 0, 1, 1);
                 INSERT INTO inputs (
                    input_id, session_id, user_message_id, state, input_kind, accepted_at_ms,
                    origin
                 ) VALUES (
                    'command-invalid', 's-invalid', 'message-invalid', 'queued', 'command', 2,
                    'runtime'
                 );
                 PRAGMA ignore_check_constraints = OFF;",
            )
            .expect("inject incompatible row");
        assert!(
            initialize(&mut connection).is_err(),
            "startup must fail closed instead of treating the command as a message"
        );
    }
}
