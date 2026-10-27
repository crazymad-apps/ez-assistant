//! v0.28.0 最终目标结构：Workspace、插件和自定义智能体的用户域事实。

use rusqlite::Connection;

use super::{Migration, MigrationError, Result, v0_27_0};

#[cfg(test)]
mod tests;

pub(super) fn entry() -> Migration {
    Migration {
        key: "release",
        version: "0.28.0",
        min_compatible_host_version: Some("0.28.0"),
        rebuilds_referenced_tables: true,
        validate_source: v0_27_0::validate,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    let before: i64 =
        connection.query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))?;
    connection.execute_batch(
        "CREATE TABLE workspaces_v0_28_new (
            workspace_id       TEXT PRIMARY KEY,
            label              TEXT NOT NULL,
            user_directory     TEXT UNIQUE,
            additional_directories_json TEXT NOT NULL DEFAULT '[]'
                CHECK (json_valid(additional_directories_json)
                    AND json_type(additional_directories_json) = 'array'
                    AND (user_directory IS NOT NULL OR json_array_length(additional_directories_json) = 0)),
            agent_directory    TEXT NOT NULL UNIQUE,
            lifecycle          TEXT NOT NULL CHECK (lifecycle IN ('active', 'removed')),
            created_at_ms      INTEGER NOT NULL,
            updated_at_ms      INTEGER NOT NULL,
            removed_at_ms      INTEGER
        );
        INSERT INTO workspaces_v0_28_new (
            workspace_id, label, user_directory, additional_directories_json,
            agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
        )
        SELECT workspace_id, label, user_directory, additional_directories_json,
               agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
        FROM workspaces;",
    )?;
    let copied: i64 =
        connection.query_row("SELECT COUNT(*) FROM workspaces_v0_28_new", [], |row| {
            row.get(0)
        })?;
    let differs: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT workspace_id, label, user_directory, additional_directories_json,
                   agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
            FROM workspaces
            EXCEPT
            SELECT workspace_id, label, user_directory, additional_directories_json,
                   agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
            FROM workspaces_v0_28_new
        ) OR EXISTS(
            SELECT workspace_id, label, user_directory, additional_directories_json,
                   agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
            FROM workspaces_v0_28_new
            EXCEPT
            SELECT workspace_id, label, user_directory, additional_directories_json,
                   agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
            FROM workspaces
        )",
        [],
        |row| row.get(0),
    )?;
    if copied != before || differs {
        return Err(MigrationError::Integrity);
    }
    connection.execute_batch(
        "DROP TABLE workspaces;
         ALTER TABLE workspaces_v0_28_new RENAME TO workspaces;
         CREATE TABLE plugins (
             name TEXT PRIMARY KEY NOT NULL,
             source_kind TEXT NOT NULL CHECK(source_kind IN ('upload', 'center')),
             source_center_id TEXT,
             active_version TEXT,
             active_sha256 TEXT,
             active_manifest_json TEXT,
             enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
             parameters_json TEXT NOT NULL DEFAULT '{}'
                 CHECK(json_valid(parameters_json) AND json_type(parameters_json) = 'object'),
             software_paths_json TEXT NOT NULL DEFAULT '{}'
                 CHECK(json_valid(software_paths_json) AND json_type(software_paths_json) = 'object'),
             market_status TEXT,
             created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
             updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
             CHECK((source_kind = 'upload' AND source_center_id IS NULL)
                OR (source_kind = 'center' AND source_center_id IS NOT NULL)),
             CHECK((active_version IS NULL AND active_sha256 IS NULL AND active_manifest_json IS NULL)
                OR (active_version IS NOT NULL AND active_sha256 IS NOT NULL
                    AND active_manifest_json IS NOT NULL
                    AND json_valid(active_manifest_json)))
         );
         CREATE TABLE plugin_operations (
             operation_key TEXT PRIMARY KEY NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('install', 'update', 'uninstall')),
             intent_json TEXT NOT NULL CHECK(json_valid(intent_json)),
             phase TEXT NOT NULL CHECK(phase IN (
                 'accepted', 'preparing', 'committing', 'cleaning',
                 'succeeded', 'failed', 'cancelled', 'interrupted')),
             committed_json TEXT NOT NULL DEFAULT '[]'
                 CHECK(json_valid(committed_json) AND json_type(committed_json) = 'array'),
             cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0, 1)),
             error_code TEXT,
             created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
             updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
         );
         CREATE TABLE custom_agents (
             name TEXT PRIMARY KEY NOT NULL,
             config_json TEXT NOT NULL
                 CHECK(json_valid(config_json) AND json_type(config_json) = 'object'),
             created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
             updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= created_at_ms)
         );
         ALTER TABLE sessions ADD COLUMN custom_agent_json TEXT
             CHECK(custom_agent_json IS NULL OR
                   (json_valid(custom_agent_json) AND json_type(custom_agent_json) = 'object'));",
    )?;
    Ok(())
}

pub(super) fn validate(connection: &Connection) -> Result<()> {
    v0_27_0::validate(connection)?;
    validate_additions(connection)
}

pub(super) fn validate_additions(connection: &Connection) -> Result<()> {
    let columns: Vec<(String, i64)> = connection
        .prepare("SELECT name, \"notnull\" FROM pragma_table_info('workspaces') ORDER BY cid")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if columns.iter().find(|(name, _)| name == "user_directory")
        != Some(&("user_directory".to_owned(), 0))
    {
        return Err(MigrationError::Integrity);
    }
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspaces
         WHERE json_valid(additional_directories_json)=0
            OR json_type(additional_directories_json)!='array'
            OR (user_directory IS NULL AND json_array_length(additional_directories_json)!=0))
         OR EXISTS(SELECT 1 FROM sqlite_schema WHERE name='workspaces_v0_28_new')
         OR EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(MigrationError::Integrity);
    }
    for table in ["plugins", "plugin_operations", "custom_agents"] {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
            [table],
            |row| row.get(0),
        )?;
        if count != 1 {
            return Err(MigrationError::Integrity);
        }
    }
    connection.prepare(
        "SELECT name, source_kind, source_center_id, active_version, active_sha256,
                active_manifest_json, enabled, parameters_json, software_paths_json,
                market_status, created_at_ms, updated_at_ms FROM plugins LIMIT 0",
    )?;
    connection.prepare(
        "SELECT operation_key, kind, intent_json, phase, committed_json,
                cancel_requested, error_code, created_at_ms, updated_at_ms
         FROM plugin_operations LIMIT 0",
    )?;
    connection.prepare(
        "SELECT name, config_json, created_at_ms, updated_at_ms FROM custom_agents LIMIT 0",
    )?;
    connection.prepare("SELECT custom_agent_json FROM sessions LIMIT 0")?;
    Ok(())
}
