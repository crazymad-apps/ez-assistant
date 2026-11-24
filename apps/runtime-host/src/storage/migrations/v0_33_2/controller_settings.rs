//! 助理会话（Controller）系统提示词与一次性/限次预授权账本。

use rusqlite::Connection;

use super::super::{Migration, MigrationError, Result};

pub(super) const KEY: &str = "controller_settings_v1";

pub(in crate::storage::migrations) fn entry() -> Migration {
    Migration {
        key: KEY,
        version: "0.33.2",
        min_compatible_host_version: Some("0.33.2"),
        rebuilds_referenced_tables: false,
        validate_source: super::validate_schedule_delivery,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(include_str!("controller_settings.sql"))?;
    Ok(())
}

fn validate(connection: &Connection) -> Result<()> {
    super::validate_schedule_delivery(connection)?;
    let prompt_columns: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('controller_prompt')
         WHERE name IN ('singleton_key', 'enabled', 'content', 'revision', 'updated_at_ms')",
        [],
        |row| row.get(0),
    )?;
    if prompt_columns != 5 {
        return Err(MigrationError::Integrity);
    }
    let prompt_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM controller_prompt", [], |row| {
            row.get(0)
        })?;
    if prompt_count != 1 {
        return Err(MigrationError::Integrity);
    }
    let valid_prompt_row: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM controller_prompt
            WHERE singleton_key = 1
              AND enabled IN (0, 1)
              AND typeof(content) = 'text'
              AND typeof(revision) = 'integer' AND revision >= 0
              AND typeof(updated_at_ms) = 'integer' AND updated_at_ms >= 0
        )",
        [],
        |row| row.get(0),
    )?;
    if !valid_prompt_row {
        return Err(MigrationError::Integrity);
    }
    let grant_columns: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('controller_permission_grants')
         WHERE name IN (
             'grant_id', 'scope', 'variants_json', 'matcher_json',
             'max_uses', 'remaining_uses', 'expires_at_ms', 'note',
             'created_at_ms', 'consumed_at_ms'
         )",
        [],
        |row| row.get(0),
    )?;
    if grant_columns != 10 {
        return Err(MigrationError::Integrity);
    }
    let has_index: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type = 'index'
              AND name = 'idx_controller_permission_grants_active'
              AND tbl_name = 'controller_permission_grants'
        )",
        [],
        |row| row.get(0),
    )?;
    if !has_index {
        return Err(MigrationError::Integrity);
    }
    let invalid_grants: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM controller_permission_grants
            WHERE scope != 'controller'
               OR typeof(max_uses) != 'integer' OR max_uses <= 0
               OR typeof(remaining_uses) != 'integer' OR remaining_uses < 0 OR remaining_uses > max_uses
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid_grants {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::migrations::{migrate, product_manifest};
    use std::{fs, path::Path};

    #[test]
    fn controller_settings_migration_seeds_prompt_creates_grants_table_and_rolls_back_on_error() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        fs::create_dir_all(&root).unwrap();
        let home = tempfile::Builder::new()
            .prefix("controller-settings-migration-")
            .tempdir_in(&root)
            .unwrap();
        let entries = product_manifest();
        let index = entries
            .iter()
            .position(|entry| entry.version == "0.33.2" && entry.key == KEY)
            .unwrap();
        let previous = &entries[..index];
        let target = &entries[..=index];

        migrate(home.path(), "0.33.2", previous).unwrap();
        let report = migrate(home.path(), "0.33.2", target).unwrap();
        assert_eq!(report.applied, ["0.33.2"]);

        let db = Connection::open(home.path().join("data/runtime.sqlite3")).unwrap();
        validate(&db).unwrap();
        let prompt: (i64, String, i64, i64) = db
            .query_row(
                "SELECT enabled, content, revision, updated_at_ms
                 FROM controller_prompt WHERE singleton_key = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(prompt, (0, String::new(), 0, 0));
        assert!(
            migrate(home.path(), "0.33.2", target)
                .unwrap()
                .applied
                .is_empty()
        );
        drop(db);

        let rollback_home = tempfile::Builder::new()
            .prefix("controller-settings-rollback-")
            .tempdir_in(&root)
            .unwrap();
        migrate(rollback_home.path(), "0.33.2", previous).unwrap();
        let db = Connection::open(rollback_home.path().join("data/runtime.sqlite3")).unwrap();
        let before: i64 = db
            .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        drop(db);
        let mut failing = target.to_vec();
        failing[index].validate = |_| Err(MigrationError::Integrity);
        assert!(migrate(rollback_home.path(), "0.33.2", &failing).is_err());

        let db = Connection::open(rollback_home.path().join("data/runtime.sqlite3")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('controller_prompt', 'controller_permission_grants')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
