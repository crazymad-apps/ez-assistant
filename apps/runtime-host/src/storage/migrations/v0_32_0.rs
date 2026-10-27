//! 组网能力不改变用户业务表；登记发布版本并保持 v0.30.0 的存储兼容下限。
use super::{Migration, v0_30_0};

pub(super) fn entry() -> Migration {
    Migration {
        key: "release",
        version: "0.32.0",
        min_compatible_host_version: Some("0.30.0"),
        rebuilds_referenced_tables: false,
        validate_source: v0_30_0::validate,
        apply: |_| Ok(()),
        validate: v0_30_0::validate,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{align, backup, compatibility, manifest, migrate};
    use rusqlite::{Connection, OpenFlags};

    #[test]
    fn release_upgrade_preserves_all_tables_and_verified_backup() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        std::fs::create_dir_all(&root).unwrap();
        // 新建独立 SQLite：无网络端口，不访问用户目录。升级只增加一条版本登记。
        let home = tempfile::Builder::new()
            .prefix("v032-release-")
            .tempdir_in(root)
            .unwrap();
        let entries = manifest();
        migrate(home.path(), "0.31.0", &entries[..10]).unwrap();
        let path = home.path().join("data/runtime.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute("INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms,context_management_mode) VALUES('release-fixture','保留会话','[]','[]','active',1,0,1,1,'local_summary')", []).unwrap();
        let before = backup::evidence(&old).unwrap();
        drop(old);

        let report = migrate(home.path(), "0.32.0", &entries[..11]).unwrap();
        assert_eq!(report.applied, ["0.32.0"]);
        let backup_path = report.backup.unwrap().join("runtime.sqlite3");
        let saved =
            Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        backup::check_integrity(&saved).unwrap();
        assert_eq!(backup::evidence(&saved).unwrap(), before);
        drop(saved);

        let current = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut after = backup::evidence(&current).unwrap();
        let mut expected = before;
        // 除登记表外，全部表的精确行数、全字段摘要与 Schema 必须保持一致。
        after.tables.remove("schema_migrations");
        expected.tables.remove("schema_migrations");
        assert_eq!(after, expected);
        assert_eq!(
            compatibility::read(&current).unwrap().unwrap().to_string(),
            "0.30.0"
        );
        let registered: i64 = current
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version='0.32.0'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(registered, 1);
        drop(current);
        let repeated = migrate(home.path(), "0.32.0", &entries[..11]).unwrap();
        assert!(repeated.applied.is_empty());
        assert!(repeated.backup.is_none());
    }

    #[test]
    fn compiled_release_initializes_and_reopens_a_new_database() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        std::fs::create_dir_all(&root).unwrap();
        let home = tempfile::Builder::new()
            .prefix("release-startup-")
            .tempdir_in(root)
            .unwrap();
        align(home.path(), |_| {}).unwrap();
        align(home.path(), |_| {}).unwrap();
        let current = Connection::open_with_flags(
            home.path().join("data/runtime.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let registered: i64 = current
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version=?1 AND migration_key='release'",
                [env!("CARGO_PKG_VERSION")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(registered, 1);
        assert_eq!(
            current
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
