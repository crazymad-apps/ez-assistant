//! 明确的 v0.25.0 人工库夹具，仅用于检验初始迁移保留行为。
//! SQLite schema 初始化与当前格式核验。

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::storage::{StorageResult, internal_error};

const SCHEMA: &str = include_str!("legacy_v0_25_0.sql");

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
