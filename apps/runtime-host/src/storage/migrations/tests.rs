//! 仅使用 TempDir 创建的 SQLite。版本/目标 SQL 是测试 manifest，不注册为产品发布记录。

#[path = "tests/legacy_schema.rs"]
mod legacy_schema;

use super::*;

fn db(home: &Path) -> Connection {
    let path = home.join(DATA_DIRECTORY).join(DATABASE_FILE);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(path).unwrap()
}

fn baseline(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
        session_id TEXT PRIMARY KEY, model_key TEXT NOT NULL, title TEXT NOT NULL,
        body_generation INTEGER NOT NULL, message_count INTEGER NOT NULL
    )",
    )?;
    let before: i64 = connection.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
    connection.execute_batch(include_str!("tests_model_reference.sql"))?;
    let after: i64 = connection.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
    if before != after {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}
fn validate_baseline(connection: &Connection) -> Result<()> {
    connection.prepare("SELECT session_id, title, body_generation, message_count, model_provider_instance_id, model_id FROM sessions LIMIT 0")?;
    let old: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'model_key')",
        [],
        |r| r.get(0),
    )?;
    if old {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}
fn first() -> Migration {
    Migration {
        version: BASELINE,
        apply: baseline,
        validate: validate_baseline,
    }
}
fn empty(_: &Connection) -> Result<()> {
    Ok(())
}
fn next(connection: &Connection) -> Result<()> {
    connection.execute_batch("ALTER TABLE sessions ADD COLUMN extension TEXT")?;
    Ok(())
}
fn ledger(connection: &Connection) -> Vec<String> {
    connection
        .prepare("SELECT version FROM schema_migrations ORDER BY applied_at_ms, rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
fn legacy(connection: &Connection) {
    connection.execute_batch("CREATE TABLE sessions(session_id TEXT PRIMARY KEY, model_key TEXT NOT NULL, title TEXT NOT NULL, body_generation INTEGER NOT NULL, message_count INTEGER NOT NULL);
        INSERT INTO sessions VALUES('one','unparseable old key','保留历史',2,9)").unwrap();
}

#[test]
fn new_database_and_repeated_start_only_apply_once() {
    let home = tempfile::tempdir().unwrap();
    let result = migrate(home.path(), BASELINE, &[first()]).unwrap();
    assert!(result.new_database);
    assert!(result.backup.is_none());
    assert_eq!(result.applied, [BASELINE]);
    let connection = db(home.path());
    assert_eq!(ledger(&connection), [BASELINE]);
    connection
        .execute("INSERT INTO providers VALUES ('p')", [])
        .unwrap();
    let again = migrate(home.path(), BASELINE, &[first()]).unwrap();
    assert!(again.applied.is_empty());
    assert!(again.backup.is_none());
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM providers", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn unversioned_and_empty_ledger_preserve_history_and_verify_independent_backup() {
    for empty_ledger in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let connection = db(home.path());
        legacy(&connection);
        if empty_ledger {
            connection.execute_batch(LEDGER).unwrap();
        }
        fs::write(
            home.path().join("config.toml"),
            "legacy = 'kept only in backup'",
        )
        .unwrap();
        let result = migrate(home.path(), BASELINE, &[first()]).unwrap();
        let backup_path = result.backup.unwrap();
        let backup = Connection::open_with_flags(
            backup_path.join("runtime.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            backup
                .query_row("SELECT model_key FROM sessions", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "unparseable old key"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT title, body_generation, message_count, model_id FROM sessions",
                    [],
                    |r| Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?
                    ))
                )
                .unwrap(),
            ("保留历史".into(), 2, 9, None)
        );
        assert_eq!(
            fs::read(home.path().join("config.toml")).unwrap(),
            fs::read(backup_path.join("config.toml")).unwrap()
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(backup_path.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["database"]["tables"]["sessions"]["rows"], 1);
        assert_eq!(manifest["external_files_included"], false);
        assert!(
            connection
                .execute("UPDATE sessions SET model_id='orphan'", [])
                .is_err()
        );
    }
}

#[test]
fn legacy_product_schema_keeps_all_session_history_fields_after_upgrade() {
    let home = tempfile::tempdir().unwrap();
    let mut connection = db(home.path());
    legacy_schema::initialize(&mut connection).unwrap();
    connection.execute("INSERT INTO sessions(session_id,title,model_key,system_prompt_json,skill_catalog_json,lifecycle,created_at_ms,updated_at_ms,body_generation,message_count)
        VALUES('s','原会话','old-model','[]','{}','active',1,2,3,4)", []).unwrap();
    let before: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    migrate(home.path(), BASELINE, &[first()]).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT session_id,system_prompt_json,body_generation,message_count FROM sessions",
                [],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?
                ))
            )
            .unwrap(),
        ("s".into(), "[]".into(), 3, 4)
    );
    let after: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, before + 3); // 仅账本与两个测试目标表；不是正式 v0.25.1 全量 DDL。
}

#[test]
fn skips_committed_prefix_and_records_explicit_empty_versions_in_semver_order() {
    let home = tempfile::tempdir().unwrap();
    migrate(home.path(), BASELINE, &[first()]).unwrap();
    let manifest = [
        first(),
        Migration {
            version: "0.25.2",
            apply: next,
            validate: empty,
        },
        Migration {
            version: "0.25.10",
            apply: empty,
            validate: empty,
        },
    ];
    let report = migrate(home.path(), "0.25.10", &manifest).unwrap();
    assert_eq!(report.applied, ["0.25.2", "0.25.10"]);
    assert_eq!(ledger(&db(home.path())), [BASELINE, "0.25.2", "0.25.10"]);
}

#[test]
fn failure_rolls_back_current_sql_and_ledger_then_stops_successors() {
    fn fail(c: &Connection) -> Result<()> {
        c.execute_batch("CREATE TABLE partial(id); INSERT INTO missing VALUES(1)")?;
        Ok(())
    }
    let home = tempfile::tempdir().unwrap();
    let manifest = [
        first(),
        Migration {
            version: "0.25.2",
            apply: fail,
            validate: empty,
        },
        Migration {
            version: "0.25.3",
            apply: next,
            validate: empty,
        },
    ];
    let startup = crate::http::StartupStateHandle::new();
    let failed = migrate_with_progress(home.path(), "0.25.3", &manifest, |progress| {
        startup.database_progress(progress)
    });
    assert!(
        matches!(failed, Err(MigrationError::VersionFailed {version,..}) if version == "0.25.2")
    );
    startup.fail(assistant_protocol::RuntimeHostStartupError::MigrationFailed);
    assert_eq!(startup.health().database_version.as_deref(), Some(BASELINE));
    let connection = db(home.path());
    assert_eq!(ledger(&connection), [BASELINE]);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(connection.prepare("SELECT * FROM partial").is_err());
    assert!(
        connection
            .prepare("SELECT extension FROM sessions")
            .is_err()
    );
    // 显式再次启动只运行尚未提交的版本，不重跑会 DROP COLUMN 的基线。
    migrate(
        home.path(),
        "0.25.2",
        &[
            first(),
            Migration {
                version: "0.25.2",
                apply: next,
                validate: empty,
            },
        ],
    )
    .unwrap();
    assert_eq!(ledger(&connection), [BASELINE, "0.25.2"]);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn validation_failure_and_illegal_transaction_controls_roll_back_initial_ledger() {
    fn rejected(_: &Connection) -> Result<()> {
        Err(MigrationError::Integrity)
    }
    fn commit(c: &Connection) -> Result<()> {
        c.execute_batch("CREATE TABLE partial(id); COMMIT;")?;
        Ok(())
    }
    fn unsafe_pragma(c: &Connection) -> Result<()> {
        c.execute_batch("PRAGMA writable_schema=ON")?;
        Ok(())
    }
    fn forged_ledger(c: &Connection) -> Result<()> {
        c.execute_batch("INSERT INTO schema_migrations VALUES('0.25.1',0)")?;
        Ok(())
    }
    for (apply, validate) in [
        (
            baseline as fn(&Connection) -> Result<()>,
            rejected as fn(&Connection) -> Result<()>,
        ),
        (commit, empty),
        (unsafe_pragma, empty),
        (forged_ledger, empty),
    ] {
        let home = tempfile::tempdir().unwrap();
        assert!(
            migrate(
                home.path(),
                BASELINE,
                &[Migration {
                    version: BASELINE,
                    apply,
                    validate
                }]
            )
            .is_err()
        );
        let c = db(home.path());
        assert_eq!(
            c.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn rejects_ledger_gaps_unknown_versions_invalid_values_and_newer_database_before_backup() {
    for (versions, newer) in [
        (vec!["0.25.2"], false),
        (vec!["0.24.9"], false),
        (vec!["v0.25.1"], false),
        (vec!["0.25.1+build"], false),
        (vec!["0.26.0"], true),
    ] {
        let home = tempfile::tempdir().unwrap();
        let c = db(home.path());
        c.execute_batch(LEDGER).unwrap();
        for v in versions {
            c.execute("INSERT INTO schema_migrations VALUES(?1,0)", [v])
                .unwrap();
        }
        let error = migrate(
            home.path(),
            "0.25.2",
            &[
                first(),
                Migration {
                    version: "0.25.2",
                    apply: empty,
                    validate: empty,
                },
            ],
        )
        .unwrap_err();
        assert_eq!(matches!(error, MigrationError::NewerDatabase), newer);
        assert!(!home.path().join("backups").exists());
        assert!(c.prepare("SELECT * FROM sessions").is_err());
    }
}

#[test]
fn invalid_manifest_is_rejected_without_creating_database() {
    let home = tempfile::tempdir().unwrap();
    for manifest in [
        vec![],
        vec![first(), first()],
        vec![Migration {
            version: "0.25.2",
            apply: empty,
            validate: empty,
        }],
    ] {
        assert!(matches!(
            migrate(home.path(), BASELINE, &manifest),
            Err(MigrationError::InvalidManifest)
        ));
    }
    assert!(migrate(home.path(), "0.25.2", &[first()]).is_err());
    assert!(!home.path().join(DATA_DIRECTORY).exists());
}

#[test]
fn backup_failure_or_corrupt_existing_file_never_creates_ledger_or_changes_source() {
    let home = tempfile::tempdir().unwrap();
    let c = db(home.path());
    legacy(&c);
    fs::write(home.path().join("backups"), "not a directory").unwrap();
    assert!(migrate(home.path(), BASELINE, &[first()]).is_err());
    assert!(c.prepare("SELECT * FROM schema_migrations").is_err());
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let corrupt = tempfile::tempdir().unwrap();
    fs::create_dir(corrupt.path().join(DATA_DIRECTORY)).unwrap();
    let path = corrupt.path().join(DATA_DIRECTORY).join(DATABASE_FILE);
    fs::write(&path, b"not sqlite").unwrap();
    assert!(migrate(corrupt.path(), BASELINE, &[first()]).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"not sqlite");
}

#[test]
fn referenced_old_column_fails_without_partial_schema_changes() {
    let home = tempfile::tempdir().unwrap();
    let c = db(home.path());
    legacy(&c);
    c.execute_batch("CREATE INDEX old_reference ON sessions(model_key)")
        .unwrap();
    let error = migrate(home.path(), BASELINE, &[first()]).unwrap_err();
    assert!(matches!(
        error,
        MigrationError::VersionFailed {
            backup: Some(_),
            ..
        }
    ));
    assert!(c.prepare("SELECT model_key FROM sessions").is_ok());
    assert!(c.prepare("SELECT model_id FROM sessions").is_err());
    assert!(c.prepare("SELECT * FROM schema_migrations").is_err());
}

#[test]
fn backup_contains_wal_rows_and_fixed_constraints_are_enforced() {
    let home = tempfile::tempdir().unwrap();
    let c = db(home.path());
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    c.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    legacy(&c);
    let report = migrate(home.path(), BASELINE, &[first()]).unwrap();
    let b = Connection::open_with_flags(
        report.backup.unwrap().join("runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        b.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    c.pragma_update(None, "foreign_keys", true).unwrap();
    c.execute("INSERT INTO providers VALUES('p')", []).unwrap();
    c.execute(
        "INSERT INTO model_fixed_configs VALUES('p','m',8192,1024)",
        [],
    )
    .unwrap();
    assert!(
        c.execute(
            "INSERT INTO model_fixed_configs VALUES('p','bad',9007199254740991,4294967296)",
            []
        )
        .is_err()
    );
    c.execute("DELETE FROM providers WHERE provider_instance_id='p'", [])
        .unwrap();
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM model_fixed_configs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn aligned_version_still_rejects_damaged_projection_without_reapplying_sql() {
    let home = tempfile::tempdir().unwrap();
    migrate(home.path(), BASELINE, &[first()]).unwrap();
    let connection = db(home.path());
    connection
        .execute_batch("ALTER TABLE sessions DROP COLUMN title")
        .unwrap();
    assert!(migrate(home.path(), BASELINE, &[first()]).is_err());
    assert_eq!(ledger(&connection), [BASELINE]);
    assert!(!home.path().join("backups").exists());
}

#[test]
fn validation_cannot_mutate_business_rows_or_ledger() {
    fn mutating(connection: &Connection) -> Result<()> {
        connection.execute("DELETE FROM providers", [])?;
        Ok(())
    }
    fn commits(connection: &Connection) -> Result<()> {
        connection.execute_batch("COMMIT")?;
        Ok(())
    }
    for validate in [mutating as fn(&Connection) -> Result<()>, commits] {
        let home = tempfile::tempdir().unwrap();
        assert!(
            migrate(
                home.path(),
                BASELINE,
                &[Migration {
                    version: BASELINE,
                    apply: baseline,
                    validate
                }]
            )
            .is_err()
        );
        let connection = db(home.path());
        assert!(
            connection
                .prepare("SELECT * FROM schema_migrations")
                .is_err()
        );
        assert!(connection.prepare("SELECT * FROM providers").is_err());
    }
}

#[test]
fn ledger_with_missing_middle_release_or_wrong_shape_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    let connection = db(home.path());
    connection.execute_batch(LEDGER).unwrap();
    connection
        .execute_batch("INSERT INTO schema_migrations VALUES ('0.25.1',1),('0.25.3',3)")
        .unwrap();
    let manifest = [
        first(),
        Migration {
            version: "0.25.2",
            apply: empty,
            validate: empty,
        },
        Migration {
            version: "0.25.3",
            apply: empty,
            validate: empty,
        },
    ];
    assert!(matches!(
        migrate(home.path(), "0.25.3", &manifest),
        Err(MigrationError::InvalidLedger)
    ));
    let malformed = tempfile::tempdir().unwrap();
    db(malformed.path()).execute_batch("CREATE TABLE schema_migrations(version TEXT, applied_at_ms INTEGER); INSERT INTO schema_migrations VALUES('0.25.1',0),('0.25.1',0)").unwrap();
    assert!(matches!(
        migrate(malformed.path(), BASELINE, &[first()]),
        Err(MigrationError::InvalidLedger)
    ));
}

#[test]
fn controlled_upgrade_reports_backup_and_failed_sql_without_enabling_runtime() {
    use assistant_protocol::{
        RuntimeHostHealthStatus, RuntimeHostStartupError, RuntimeHostStartupStage,
    };
    let home = tempfile::tempdir().unwrap();
    legacy(&db(home.path()));
    let startup = crate::http::StartupStateHandle::new();
    let mut stages = Vec::new();
    let failing = Migration {
        version: BASELINE,
        apply: |connection| {
            connection
                .execute_batch("DELETE FROM sessions; SELECT * FROM intentional_missing_table")?;
            Ok(())
        },
        validate: empty,
    };
    let error = migrate_with_progress(home.path(), BASELINE, &[failing], |progress| {
        let stage = progress.stage;
        stages.push(stage);
        startup.database_progress(progress);
        assert_eq!(startup.health().status, RuntimeHostHealthStatus::Starting);
        assert_eq!(startup.health().stage, Some(stage));
    })
    .unwrap_err();
    assert!(matches!(error, MigrationError::VersionFailed { .. }));
    startup.fail(RuntimeHostStartupError::MigrationFailed);
    assert_eq!(
        stages,
        vec![
            RuntimeHostStartupStage::DatabaseCheck,
            RuntimeHostStartupStage::DatabaseBackup,
            RuntimeHostStartupStage::DatabaseMigration
        ]
    );
    let MigrationError::VersionFailed {
        backup: Some(directory),
        ..
    } = &error
    else {
        panic!("verified backup expected");
    };
    let backup = Connection::open_with_flags(
        directory.join("runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let health = startup.health();
    assert!(health.database_version.is_none());
    assert_eq!(health.status, RuntimeHostHealthStatus::Unavailable);
    assert_eq!(health.error, Some(RuntimeHostStartupError::MigrationFailed));
    startup.stage(RuntimeHostStartupStage::Recovery);
    assert_eq!(
        startup.health(),
        health,
        "failed startup cannot silently resume"
    );
    let connection = db(home.path());
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sessions WHERE session_id='one' AND model_key='unparseable old key' AND title='保留历史' AND message_count=9", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='schema_migrations'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn product_baseline_creates_full_schema_and_removes_legacy_model_fields_atomically() {
    let home = tempfile::tempdir().unwrap();
    let mut connection = db(home.path());
    legacy_schema::initialize(&mut connection).unwrap();
    connection.execute_batch("INSERT INTO sessions(session_id,title,model_key,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms) VALUES ('legacy','history','invalid model !','[]','{}','active',1,7,1,2)").unwrap();
    drop(connection);
    let report = migrate(home.path(), BASELINE, &[v0_25_1::entry()]).unwrap();
    assert_eq!(report.applied, vec![BASELINE]);
    let backup = report.backup.unwrap();
    assert!(backup.is_dir());
    let connection = db(home.path());
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sessions WHERE session_id='legacy' AND title='history' AND message_count=7 AND body_generation=1 AND model_provider_instance_id IS NULL AND model_id IS NULL", [], |r|r.get::<_,i64>(0)).unwrap(),1);
    for table in ["providers", "model_fixed_configs"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM model_settings WHERE singleton_key=1 AND default_model_id IS NULL AND vision_model_id IS NULL",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(ledger(&connection), vec![BASELINE]);
    assert!(
        connection
            .prepare("SELECT model_key FROM sessions")
            .is_err()
    );
    drop(connection);
    assert!(
        migrate(home.path(), BASELINE, &[v0_25_1::entry()])
            .unwrap()
            .applied
            .is_empty()
    );
    let fresh = tempfile::tempdir().unwrap();
    assert!(
        migrate(fresh.path(), BASELINE, &[v0_25_1::entry()])
            .unwrap()
            .new_database
    );
}

#[test]
fn baseline_discards_legacy_empty_work_plan_once_and_preserves_history() {
    let home = tempfile::tempdir().unwrap();
    let mut connection = db(home.path());
    legacy_schema::initialize(&mut connection).unwrap();
    connection.execute_batch("INSERT INTO sessions(session_id,title,model_key,system_prompt_json,skill_catalog_json,lifecycle,created_at_ms,updated_at_ms,body_generation,message_count)
        VALUES('legacy-plan','保留标题','discarded','[]','{}','archived',1,2,3,4);
        INSERT INTO session_work_plans(session_id,revision,objective,items_json,last_operation_id,updated_at_ms)
        VALUES('legacy-plan',1,'empty','[]','old-operation',1);").unwrap();
    drop(connection);
    let outcome = migrate(home.path(), BASELINE, &[v0_25_1::entry()]).unwrap();
    assert!(outcome.backup.is_some());
    let connection = db(home.path());
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_work_plans", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT title,lifecycle,body_generation,message_count FROM sessions",
                [],
                |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?
                ))
            )
            .unwrap(),
        ("保留标题".into(), "archived".into(), 3, 4)
    );
    assert!(
        migrate(home.path(), BASELINE, &[v0_25_1::entry()])
            .unwrap()
            .applied
            .is_empty()
    );
}
