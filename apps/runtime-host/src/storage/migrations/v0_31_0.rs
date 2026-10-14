//! 本版不改变业务表；仍登记发布版本，保留 v0.30.0 的存储兼容下限。
use super::{Migration, v0_30_0};

pub(super) fn entry() -> Migration {
    Migration {
        version: "0.31.0",
        min_compatible_host_version: Some("0.30.0"),
        rebuilds_referenced_tables: false,
        validate_source: v0_30_0::validate,
        apply: |_| Ok(()),
        validate: v0_30_0::validate,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{manifest, migrate};
    use rusqlite::{Connection, OpenFlags};

    #[test]
    fn release_upgrade_preserves_session_and_is_idempotent() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        std::fs::create_dir_all(&root).unwrap();
        // 本次独立 SQLite 夹具，无网络端口，不读取用户 Runtime Home。
        let home = tempfile::Builder::new()
            .prefix("v031-release-")
            .tempdir_in(root)
            .unwrap();
        let manifest = manifest();
        migrate(home.path(), "0.30.0", &manifest[..9]).unwrap();
        let path = home.path().join("data/runtime.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute("INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms,context_management_mode) VALUES('release-fixture','保留会话','[]','[]','active',1,0,1,1,'local_summary')", []).unwrap();
        drop(old);
        let report = migrate(home.path(), "0.31.0", &manifest).unwrap();
        assert_eq!(report.applied, ["0.31.0"]);
        let backup = report.backup.unwrap().join("runtime.sqlite3");
        for database in [&path, &backup] {
            let connection =
                Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let row: (i64, String, String) = connection.query_row("SELECT COUNT(*),title,context_management_mode FROM sessions WHERE session_id='release-fixture'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
            assert_eq!(row, (1, "保留会话".into(), "local_summary".into()));
        }
        let repeated = migrate(home.path(), "0.31.0", &manifest).unwrap();
        assert!(repeated.applied.is_empty());
        assert!(repeated.backup.is_none());
    }
}
