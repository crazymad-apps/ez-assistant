//! 补丁版只登记发布版本，沿用已发布的业务结构与存储兼容下限。
use super::{Migration, usage_snapshots};

pub(super) fn entry() -> Migration {
    Migration {
        key: "release",
        version: "0.32.2",
        min_compatible_host_version: Some("0.32.1"),
        rebuilds_referenced_tables: false,
        validate_source: usage_snapshots::entry().validate,
        apply: |_| Ok(()),
        validate: usage_snapshots::entry().validate,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{backup, compatibility, migrate, product_manifest};
    use rusqlite::{Connection, OpenFlags};

    #[test]
    fn patch_release_preserves_existing_data_and_backup() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        std::fs::create_dir_all(&root).unwrap();
        let home = tempfile::Builder::new()
            .prefix("v0322-release-")
            .tempdir_in(root)
            .unwrap();
        let entries = product_manifest();
        let release = entries
            .iter()
            .position(|entry| entry.version == "0.32.2")
            .unwrap();
        let entries = &entries[..=release];
        migrate(home.path(), "0.32.1", &entries[..release]).unwrap();
        let path = home.path().join("data/runtime.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute("INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms) VALUES('patch-fixture','保留会话','[]','[]','active',1,0,1,1)", []).unwrap();
        let before = backup::evidence(&old).unwrap();
        drop(old);

        let report = migrate(home.path(), "0.32.2", entries).unwrap();
        assert_eq!(report.applied, ["0.32.2"]);
        let saved = Connection::open_with_flags(
            report.backup.unwrap().join("runtime.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        backup::check_integrity(&saved).unwrap();
        assert_eq!(backup::evidence(&saved).unwrap(), before);
        let current = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut after = backup::evidence(&current).unwrap();
        let mut expected = before;
        after.tables.remove("schema_migrations");
        expected.tables.remove("schema_migrations");
        assert_eq!(after, expected);
        assert_eq!(
            compatibility::read(&current).unwrap().unwrap().to_string(),
            "0.32.1"
        );
        let count: i64 = current.query_row("SELECT COUNT(*) FROM schema_migrations WHERE version='0.32.2' AND migration_key='release'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        drop(current);
        drop(saved);
        let repeated = migrate(home.path(), "0.32.2", entries).unwrap();
        assert!(repeated.applied.is_empty());
        assert!(repeated.backup.is_none());
    }
}
