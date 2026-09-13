//! 只操作本测试创建的 TempDir SQLite；升级使用正式备份/事务执行器。

use super::super::{DATA_DIRECTORY, DATABASE_FILE, backup, compatibility, migrate, v0_25_2};
use super::*;
use rusqlite::OpenFlags;

fn open(home: &std::path::Path) -> Connection {
    Connection::open(home.join(DATA_DIRECTORY).join(DATABASE_FILE)).expect("temporary database")
}

#[test]
fn upgrade_preserves_rows_backs_up_source_and_rejects_old_host() {
    let home = tempfile::tempdir().expect("isolated runtime home");
    migrate(home.path(), "0.25.2", &[v0_25_1::entry(), v0_25_2::entry()]).expect("source schema");
    let source = open(home.path());
    source.execute_batch(
        "INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms)
         VALUES('session','中文历史','[]','{}','active',2,3,10,20);
         INSERT INTO inputs(input_id,session_id,user_message_id,state,accepted_at_ms)
         VALUES('input','session','message','committed',11);
         INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms)
         VALUES('run','session','input',1,'completed',0,12);",
    ).expect("source fixture");
    drop(source);
    let report = migrate(
        home.path(),
        "0.25.3",
        &[v0_25_1::entry(), v0_25_2::entry(), entry()],
    )
    .expect("upgrade");
    assert_eq!(report.applied, ["0.25.3"]);
    let backup_path = report.backup.expect("independent backup before migration");
    let saved = Connection::open_with_flags(
        backup_path.join("runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("read backup");
    backup::check_integrity(&saved).expect("backup integrity");
    let migrated = open(home.path());
    for table in ["sessions", "inputs", "runs"] {
        let query = format!("SELECT COUNT(*) FROM {table}");
        assert_eq!(
            saved
                .query_row(&query, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            migrated
                .query_row(&query, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    let history: (String, i64, i64) = migrated
        .query_row(
            "SELECT title,body_generation,message_count FROM sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(history, ("中文历史".to_owned(), 2, 3));
    assert!(
        saved
            .prepare("SELECT agent_shell_kind FROM sessions")
            .is_err()
    );
    assert_eq!(
        migrated
            .query_row("SELECT agent_shell_kind FROM sessions", [], |row| row
                .get::<_, Option<String>>(0))
            .unwrap(),
        None
    );
    assert_eq!(
        migrated
            .query_row("SELECT shell_snapshot_json FROM runs", [], |row| row
                .get::<_, Option<String>>(0))
            .unwrap(),
        None
    );
    assert_eq!(
        compatibility::read(&migrated).unwrap().unwrap().to_string(),
        "0.25.3"
    );
    drop(migrated);
    let repeated = migrate(
        home.path(),
        "0.25.3",
        &[v0_25_1::entry(), v0_25_2::entry(), entry()],
    )
    .unwrap();
    assert!(repeated.applied.is_empty() && repeated.backup.is_none());
    assert!(matches!(
        migrate(home.path(), "0.25.2", &[v0_25_1::entry(), v0_25_2::entry()]),
        Err(MigrationError::HostTooOld)
    ));
}

#[test]
fn new_database_has_one_default_and_constraints_reject_unknown_shell() {
    let home = tempfile::tempdir().unwrap();
    let report = migrate(
        home.path(),
        "0.25.3",
        &[v0_25_1::entry(), v0_25_2::entry(), entry()],
    )
    .unwrap();
    assert!(report.new_database && report.backup.is_none());
    let connection = open(home.path());
    assert_eq!(
        connection
            .query_row(
                "SELECT default_agent_shell_kind FROM agent_shell_settings WHERE singleton_key=1",
                [],
                |r| r.get::<_, Option<String>>(0)
            )
            .unwrap(),
        None
    );
    assert!(
        connection
            .execute(
                "UPDATE agent_shell_settings SET default_agent_shell_kind='arbitrary.exe'",
                []
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "INSERT INTO agent_shell_settings(singleton_key) VALUES(2)",
                []
            )
            .is_err()
    );
    connection
        .execute(
            "UPDATE agent_shell_settings SET default_agent_shell_kind='powershell_7'",
            [],
        )
        .unwrap();
    validate(&connection).unwrap();
}
