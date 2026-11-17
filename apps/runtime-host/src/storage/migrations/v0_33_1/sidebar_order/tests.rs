use super::*;
use crate::storage::migrations::{migrate, product_manifest};
use std::{fs, path::Path};

#[test]
fn same_version_sort_migration_preserves_history_and_rejects_early_manifest() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f04-migration-")
        .tempdir_in(root)
        .unwrap();
    let entries = product_manifest();
    let old = &entries[..entries.len() - 1];
    migrate(home.path(), "0.33.1", old).unwrap();
    let db = Connection::open(home.path().join("data/runtime.sqlite3")).unwrap();
    db.execute_batch("INSERT INTO workspaces(workspace_id,label,agent_directory,lifecycle,created_at_ms,updated_at_ms,additional_directories_json) VALUES('a','A','/fixture/a','active',1,90,'[]'),('b','B','/fixture/b','removed',2,80,'[]');
        INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms,is_pinned) VALUES
        ('old','old','{}','[]','active',1,0,1,99,1),('new','new','{}','[]','archived',1,0,2,88,1);
        INSERT INTO session_resources(session_id,workspace_id,working_directory,additional_workspace_directories_json,attachment_directory,private_directory,created_at_ms) VALUES
        ('old','a','/fixture/a','[]','/fixture/old/attachments','/fixture/old/private',1),('new','a','/fixture/a','[]','/fixture/new/attachments','/fixture/new/private',2);").unwrap();
    // 没有正文文件：仅排序升级不得误入 F02 的文件换代。
    let report = migrate(home.path(), "0.33.1", &entries).unwrap();
    assert_eq!(report.applied, ["0.33.1"]);
    let backup = report.backup.unwrap();
    let backup_db = Connection::open_with_flags(
        backup.join("runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    for table in ["sessions", "session_resources", "workspaces"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        assert_eq!(
            backup_db
                .query_row(&sql, [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(db.query_row(&sql, [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
    assert_eq!(
        db.query_row(
            "SELECT sort_order FROM session_resources WHERE session_id='new'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2048
    );
    assert_eq!(
        db.query_row(
            "SELECT pin_order FROM sessions WHERE session_id='old'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2048
    );
    validate(&db).unwrap();
    assert!(migrate(home.path(), "0.33.1", old).is_err());
    assert!(
        migrate(home.path(), "0.33.1", &entries)
            .unwrap()
            .applied
            .is_empty()
    );
}

#[test]
fn failed_sort_migration_rolls_back_columns_values_and_ledger() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f04-rollback-")
        .tempdir_in(root)
        .unwrap();
    let mut entries = product_manifest();
    migrate(home.path(), "0.33.1", &entries[..entries.len() - 1]).unwrap();
    let db = Connection::open(home.path().join("data/runtime.sqlite3")).unwrap();
    let before: i64 = db
        .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    entries.last_mut().unwrap().validate = |_| Err(MigrationError::Integrity);
    assert!(migrate(home.path(), "0.33.1", &entries).is_err());
    assert_eq!(
        db.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM pragma_table_info('sessions') WHERE name='pin_order'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM pragma_table_info('workspaces') WHERE name='sort_order'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM pragma_table_info('session_resources') WHERE name='sort_order'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        migrate(home.path(), "0.33.1", &entries[..entries.len() - 1])
            .unwrap()
            .applied
            .is_empty()
    );
}
