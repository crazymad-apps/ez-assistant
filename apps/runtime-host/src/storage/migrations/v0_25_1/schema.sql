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
    model_provider_instance_id TEXT,
    model_id TEXT CHECK ((model_provider_instance_id IS NULL AND model_id IS NULL) OR
        (model_provider_instance_id IS NOT NULL AND length(model_provider_instance_id) > 0 AND model_id IS NOT NULL AND length(model_id) > 0)),
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
