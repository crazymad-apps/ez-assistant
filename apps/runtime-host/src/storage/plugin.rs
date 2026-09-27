//! 插件账本的 SQLite 物理实现；操作相位与插件行在单个事务中提交。

use assistant_protocol::IdempotencyKey;
use assistant_runtime::{
    PluginOperationKind, PluginOperationPhase, PluginOperationUpdate, PluginRecordChange,
    PluginSettingsChange, PluginSource, StoreError, StoreErrorKind, StoredPlugin,
    StoredPluginOperation,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, conflict, database_write_error, invalid_data,
    invalid_data_with_source,
};

impl StorageEngine {
    pub(super) fn change_plugin_settings(
        &mut self,
        change: PluginSettingsChange,
    ) -> StorageResult<StoredPlugin> {
        change.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("plugin settings transaction could not start", source)
            })?;
        let (source_kind, center_id) = match &change.expected.source {
            PluginSource::Upload => ("upload", None),
            PluginSource::Center(id) => ("center", Some(id.as_str())),
        };
        let updated = tx.execute(
            "UPDATE plugins SET enabled=?1, parameters_json=?2, software_paths_json=?3, updated_at_ms=?4
             WHERE name=?5 AND source_kind=?6 AND source_center_id IS ?7
               AND active_sha256 IS ?8 AND active_version IS ?9 AND active_manifest_json IS ?10
               AND enabled=?11 AND parameters_json=?12 AND software_paths_json=?13
               AND created_at_ms=?14 AND updated_at_ms=?15",
            params![i64::from(change.next.enabled), change.next.parameters_json,
                change.next.software_paths_json, timestamp(change.next.updated_at_ms)?,
                change.expected.name, source_kind, center_id, change.expected.active_sha256,
                change.expected.active_version, change.expected.active_manifest_json,
                i64::from(change.expected.enabled), change.expected.parameters_json,
                change.expected.software_paths_json, timestamp(change.expected.created_at_ms)?,
                timestamp(change.expected.updated_at_ms)?],
        ).map_err(|source| database_write_error("plugin settings could not be changed", source))?;
        if updated != 1 {
            return Err(conflict("plugin settings changed"));
        }
        tx.commit().map_err(|source| {
            database_write_error("plugin settings could not be committed", source)
        })?;
        Ok(change.next)
    }

    pub(super) fn list_plugins(&self) -> StorageResult<Vec<StoredPlugin>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT name, source_kind, source_center_id, active_version, active_sha256,
                    active_manifest_json, enabled, parameters_json, software_paths_json,
                    market_status, created_at_ms, updated_at_ms FROM plugins ORDER BY name",
            )
            .map_err(|source| invalid_data_with_source("plugins could not be loaded", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            })
            .map_err(|source| invalid_data_with_source("plugins could not be loaded", source))?;
        rows.map(|row| {
            let (
                name,
                source_kind,
                source_center_id,
                active_version,
                active_sha256,
                active_manifest_json,
                enabled,
                parameters_json,
                software_paths_json,
                market_status,
                created,
                updated,
            ) = row.map_err(|source| invalid_data_with_source("plugin row is invalid", source))?;
            let source = match (source_kind.as_str(), source_center_id) {
                ("upload", None) => PluginSource::Upload,
                ("center", Some(id)) => PluginSource::Center(id),
                _ => return Err(invalid_data("plugin row is invalid")),
            };
            let plugin = StoredPlugin {
                name,
                source,
                active_version,
                active_sha256,
                active_manifest_json,
                enabled: match enabled {
                    0 => false,
                    1 => true,
                    _ => return Err(invalid_data("plugin row is invalid")),
                },
                parameters_json,
                software_paths_json,
                market_status,
                created_at_ms: u64::try_from(created)
                    .map_err(|_| invalid_data("plugin row is invalid"))?,
                updated_at_ms: u64::try_from(updated)
                    .map_err(|_| invalid_data("plugin row is invalid"))?,
            };
            plugin
                .validate()
                .map_err(|_| invalid_data("plugin row is invalid"))?;
            Ok(plugin)
        })
        .collect()
    }

    pub(super) fn list_plugin_operations(&self) -> StorageResult<Vec<StoredPluginOperation>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT operation_key FROM plugin_operations ORDER BY created_at_ms, operation_key",
            )
            .map_err(|source| {
                invalid_data_with_source("plugin operations could not be loaded", source)
            })?;
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|source| {
                invalid_data_with_source("plugin operations could not be loaded", source)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| {
                invalid_data_with_source("plugin operations could not be loaded", source)
            })?;
        keys.iter()
            .map(|key| {
                load_operation(&self.connection, key)?
                    .ok_or_else(|| invalid_data("plugin operation disappeared"))
            })
            .collect()
    }

    pub(super) fn accept_plugin_operation(
        &mut self,
        operation: StoredPluginOperation,
    ) -> StorageResult<StoredPluginOperation> {
        operation.validate()?;
        if operation.phase != PluginOperationPhase::Accepted
            || operation.committed_json != "[]"
            || operation.cancel_requested
            || operation.error_code.is_some()
        {
            return Err(StoreError::new(
                StoreErrorKind::InvalidInput,
                "plugin operation initial state is invalid",
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("plugin operation transaction could not start", source)
            })?;
        if let Some(existing) = load_operation(&tx, operation.key.as_str())? {
            if existing.kind == operation.kind && existing.intent_json == operation.intent_json {
                return Ok(existing);
            }
            return Err(conflict(
                "plugin operation key conflicts with another intent",
            ));
        }
        tx.execute(
            "INSERT INTO plugin_operations (operation_key, kind, intent_json, phase, committed_json,
                    cancel_requested, error_code, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![operation.key.as_str(), operation.kind.as_str(), operation.intent_json,
                operation.phase.as_str(), operation.committed_json, i64::from(operation.cancel_requested),
                operation.error_code, timestamp(operation.created_at_ms)?, timestamp(operation.updated_at_ms)?],
        ).map_err(|source| database_write_error("plugin operation could not be accepted", source))?;
        tx.commit().map_err(|source| {
            database_write_error("plugin operation could not be committed", source)
        })?;
        Ok(operation)
    }

    pub(super) fn advance_plugin_operation(
        &mut self,
        update: PluginOperationUpdate,
    ) -> StorageResult<StoredPluginOperation> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                database_write_error("plugin operation transaction could not start", source)
            })?;
        let current = load_operation(&tx, update.next.key.as_str())?
            .ok_or_else(|| conflict("plugin operation does not exist"))?;
        if current == update.next {
            return Ok(current);
        }
        update.validate_against(&current)?;
        match &update.plugin {
            PluginRecordChange::Keep => {}
            PluginRecordChange::Upsert(plugin) => {
                if let Some((source_kind, center_id, created_at_ms)) = tx.query_row(
                    "SELECT source_kind, source_center_id, created_at_ms FROM plugins WHERE name=?1",
                    [&plugin.name],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, i64>(2)?)),
                ).optional().map_err(|source| invalid_data_with_source("plugin source could not be loaded", source))? {
                    let same_source = match &plugin.source {
                        PluginSource::Upload => source_kind == "upload" && center_id.is_none(),
                        PluginSource::Center(id) => source_kind == "center" && center_id.as_deref() == Some(id),
                    };
                    if !same_source || created_at_ms != timestamp(plugin.created_at_ms)? {
                        return Err(conflict("plugin source or creation time changed"));
                    }
                }
                let (source_kind, center_id) = match &plugin.source {
                    PluginSource::Upload => ("upload", None),
                    PluginSource::Center(id) => ("center", Some(id.as_str())),
                };
                tx.execute(
                    "INSERT INTO plugins (name, source_kind, source_center_id, active_version,
                            active_sha256, active_manifest_json, enabled, parameters_json,
                            software_paths_json, market_status, created_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                     ON CONFLICT(name) DO UPDATE SET
                        source_kind=excluded.source_kind, source_center_id=excluded.source_center_id,
                        active_version=excluded.active_version, active_sha256=excluded.active_sha256,
                        active_manifest_json=excluded.active_manifest_json, enabled=excluded.enabled,
                        parameters_json=excluded.parameters_json,
                        software_paths_json=excluded.software_paths_json,
                        market_status=excluded.market_status, updated_at_ms=excluded.updated_at_ms",
                    params![plugin.name, source_kind, center_id, plugin.active_version,
                        plugin.active_sha256, plugin.active_manifest_json, i64::from(plugin.enabled),
                        plugin.parameters_json, plugin.software_paths_json, plugin.market_status,
                        timestamp(plugin.created_at_ms)?, timestamp(plugin.updated_at_ms)?],
                ).map_err(|source| database_write_error("plugin record could not be saved", source))?;
            }
            PluginRecordChange::Remove(name) => {
                tx.execute("DELETE FROM plugins WHERE name=?1", [name])
                    .map_err(|source| {
                        database_write_error("plugin record could not be removed", source)
                    })?;
            }
        }
        tx.execute(
            "UPDATE plugin_operations SET phase=?2, committed_json=?3,
                    cancel_requested=?4, error_code=?5, updated_at_ms=?6
             WHERE operation_key=?1",
            params![
                update.next.key.as_str(),
                update.next.phase.as_str(),
                update.next.committed_json,
                i64::from(update.next.cancel_requested),
                update.next.error_code,
                timestamp(update.next.updated_at_ms)?
            ],
        )
        .map_err(|source| database_write_error("plugin operation could not be advanced", source))?;
        tx.commit().map_err(|source| {
            database_write_error("plugin operation could not be committed", source)
        })?;
        Ok(update.next)
    }
}

fn load_operation(
    connection: &Connection,
    key: &str,
) -> StorageResult<Option<StoredPluginOperation>> {
    let row = connection
        .query_row(
            "SELECT operation_key, kind, intent_json, phase, committed_json, cancel_requested,
                error_code, created_at_ms, updated_at_ms
         FROM plugin_operations WHERE operation_key=?1",
            [key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|source| {
            invalid_data_with_source("plugin operation could not be loaded", source)
        })?;
    let Some((key, kind, intent_json, phase, committed_json, cancel, error_code, created, updated)) =
        row
    else {
        return Ok(None);
    };
    let operation = StoredPluginOperation {
        key: IdempotencyKey::new(key)
            .map_err(|_| invalid_data("plugin operation row is invalid"))?,
        kind: PluginOperationKind::parse(&kind)
            .ok_or_else(|| invalid_data("plugin operation row is invalid"))?,
        intent_json,
        phase: PluginOperationPhase::parse(&phase)
            .ok_or_else(|| invalid_data("plugin operation row is invalid"))?,
        committed_json,
        cancel_requested: match cancel {
            0 => false,
            1 => true,
            _ => return Err(invalid_data("plugin operation row is invalid")),
        },
        error_code,
        created_at_ms: u64::try_from(created)
            .map_err(|_| invalid_data("plugin operation row is invalid"))?,
        updated_at_ms: u64::try_from(updated)
            .map_err(|_| invalid_data("plugin operation row is invalid"))?,
    };
    operation
        .validate()
        .map_err(|_| invalid_data("plugin operation row is invalid"))?;
    Ok(Some(operation))
}

fn timestamp(value: u64) -> StorageResult<i64> {
    i64::try_from(value)
        .map_err(|_| StoreError::new(StoreErrorKind::InvalidInput, "plugin timestamp is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn isolated_home() -> tempfile::TempDir {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".runtime-test");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("c03-m0-plugin-")
            .tempdir_in(root)
            .unwrap()
    }

    fn operation() -> StoredPluginOperation {
        StoredPluginOperation {
            key: IdempotencyKey::new("install-example").unwrap(),
            kind: PluginOperationKind::Install,
            intent_json: r#"{"name":"example","sha256":"test"}"#.into(),
            phase: PluginOperationPhase::Accepted,
            committed_json: "[]".into(),
            cancel_requested: false,
            error_code: None,
            created_at_ms: 100,
            updated_at_ms: 100,
        }
    }

    fn plugin() -> StoredPlugin {
        StoredPlugin {
            name: "example".into(),
            source: PluginSource::Upload,
            active_version: Some("1.0.0".into()),
            active_sha256: Some("a".repeat(64)),
            active_manifest_json: Some(r#"{"format_version":1}"#.into()),
            enabled: true,
            parameters_json: "{}".into(),
            software_paths_json: "{}".into(),
            market_status: None,
            created_at_ms: 100,
            updated_at_ms: 101,
        }
    }

    fn advance_to(
        engine: &mut StorageEngine,
        current: &StoredPluginOperation,
        phase: PluginOperationPhase,
    ) -> StoredPluginOperation {
        let mut next = current.clone();
        next.phase = phase;
        next.updated_at_ms += 1;
        engine
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase: current.phase,
                next: next.clone(),
                plugin: PluginRecordChange::Keep,
            })
            .unwrap();
        next
    }

    #[test]
    fn operation_and_plugin_survive_restart_and_reject_other_intent() {
        let home = isolated_home();
        let mut engine = StorageEngine::open(home.path()).unwrap();
        let first = engine.accept_plugin_operation(operation()).unwrap();
        assert_eq!(engine.accept_plugin_operation(operation()).unwrap(), first);
        let mut conflicting = operation();
        conflicting.intent_json = r#"{"name":"other"}"#.into();
        assert_eq!(
            engine
                .accept_plugin_operation(conflicting)
                .unwrap_err()
                .kind(),
            StoreErrorKind::Conflict
        );

        let preparing = advance_to(&mut engine, &first, PluginOperationPhase::Preparing);
        let committing = advance_to(&mut engine, &preparing, PluginOperationPhase::Committing);
        let mut next = committing.clone();
        next.phase = PluginOperationPhase::Cleaning;
        next.updated_at_ms += 1;
        engine
            .advance_plugin_operation(PluginOperationUpdate {
                expected_phase: PluginOperationPhase::Committing,
                next: next.clone(),
                plugin: PluginRecordChange::Upsert(plugin()),
            })
            .unwrap();
        drop(engine);

        let mut reopened = StorageEngine::open(home.path()).unwrap();
        assert_eq!(reopened.list_plugins().unwrap(), vec![plugin()]);
        assert_eq!(
            reopened.list_plugin_operations().unwrap(),
            vec![next.clone()]
        );
        assert_eq!(reopened.accept_plugin_operation(operation()).unwrap(), next);
        let mut disabled = plugin();
        disabled.enabled = false;
        disabled.updated_at_ms += 1;
        assert_eq!(
            reopened
                .change_plugin_settings(PluginSettingsChange {
                    expected: plugin(),
                    next: disabled.clone(),
                })
                .unwrap(),
            disabled,
        );
        assert_eq!(
            reopened
                .change_plugin_settings(PluginSettingsChange {
                    expected: plugin(),
                    next: disabled.clone(),
                })
                .unwrap_err()
                .kind(),
            StoreErrorKind::Conflict,
        );
        drop(reopened);
        let recovered = StorageEngine::open(home.path()).unwrap();
        assert_eq!(recovered.list_plugins().unwrap(), vec![disabled]);
    }

    #[test]
    fn failed_operation_update_rolls_back_plugin_row() {
        let home = isolated_home();
        let mut engine = StorageEngine::open(home.path()).unwrap();
        let accepted = engine.accept_plugin_operation(operation()).unwrap();
        let preparing = advance_to(&mut engine, &accepted, PluginOperationPhase::Preparing);
        let current = advance_to(&mut engine, &preparing, PluginOperationPhase::Committing);
        engine
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_plugin_phase BEFORE UPDATE ON plugin_operations
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
            )
            .unwrap();
        let mut next = current.clone();
        next.phase = PluginOperationPhase::Cleaning;
        next.updated_at_ms += 1;
        assert!(
            engine
                .advance_plugin_operation(PluginOperationUpdate {
                    expected_phase: PluginOperationPhase::Committing,
                    next,
                    plugin: PluginRecordChange::Upsert(plugin()),
                })
                .is_err()
        );
        assert!(engine.list_plugins().unwrap().is_empty());
        assert_eq!(engine.list_plugin_operations().unwrap(), vec![current]);
    }
}
