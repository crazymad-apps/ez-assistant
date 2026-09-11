//! M1 人工数据库夹具：只在 TempDir 创建，拒绝路径比较实际文件，升级路径核验账本与下限。

use super::*;
use std::collections::BTreeMap;

fn manifest() -> [Migration; 2] {
    [v0_25_1::entry(), v0_25_2::entry()]
}
fn path(home: &Path) -> PathBuf {
    home.join(DATA_DIRECTORY).join(DATABASE_FILE)
}
fn open(home: &Path) -> Connection {
    Connection::open(path(home)).unwrap()
}

/// 故障注入前保留独立副本；沿用逐表数量／字段摘要核验，测试退出后仍可在隔离目录复查。
fn preserve_before_fault(home: &Path) {
    let backup_home = tempfile::Builder::new()
        .prefix("m1-before-fault-")
        .tempdir()
        .unwrap()
        .keep();
    let connection = open(home);
    let saved = backup::create(&connection, &backup_home, &path(home), "0.25.2").unwrap();
    eprintln!("verified fixture backup: {}", saved.display());
}

fn snapshot(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.insert(path.clone(), fs::read(path).unwrap());
        }
    }
    files
}

#[test]
fn new_database_and_known_upgrade_commit_minimum_once_with_verified_backup() {
    for existing in [false, true] {
        let home = tempfile::tempdir().unwrap();
        if existing {
            migrate(home.path(), BASELINE, &[v0_25_1::entry()]).unwrap();
        }
        let report = migrate(home.path(), "0.25.2", &manifest()).unwrap();
        assert_eq!(report.backup.is_some(), existing);
        let connection = open(home.path());
        assert_eq!(
            compatibility::read(&connection).unwrap(),
            Some(parse_version("0.25.2").unwrap())
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM database_compatibility WHERE id=1 AND min_compatible_host_version='0.25.2'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        if let Some(backup) = report.backup {
            let evidence: serde_json::Value =
                serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
            let copy = Connection::open_with_flags(
                backup.join("runtime.sqlite3"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            backup::check_integrity(&copy).unwrap();
            assert_eq!(
                copy.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                evidence["database"]["tables"]["schema_migrations"]["rows"],
                1
            );
            assert!(compatibility::read(&copy).unwrap().is_none());
        }
        drop(connection);
        let before = snapshot(home.path());
        let repeated = migrate(home.path(), "0.25.2", &manifest()).unwrap();
        assert!(repeated.applied.is_empty() && repeated.backup.is_none());
        assert_eq!(snapshot(home.path()), before);
    }
}

#[test]
fn high_minimum_blocks_store_and_exposes_only_safe_versions_before_business_directories() {
    let home = tempfile::tempdir().unwrap();
    migrate(home.path(), "0.25.2", &manifest()).unwrap();
    preserve_before_fault(home.path());
    open(home.path())
        .execute(
            "UPDATE database_compatibility SET min_compatible_host_version='0.25.3'",
            [],
        )
        .unwrap();
    let before = snapshot(home.path());
    let startup = crate::http::StartupStateHandle::new();
    let failure = migrate_with_progress(home.path(), "0.25.2", &manifest(), |p| {
        startup.database_progress(p)
    })
    .unwrap_err();
    assert!(matches!(failure, MigrationError::HostTooOld));
    startup.fail(assistant_protocol::RuntimeHostStartupError::DatabaseHostTooOld);
    assert_eq!(
        startup.health().min_compatible_host_version.as_deref(),
        Some("0.25.3")
    );
    assert!(super::super::engine::StorageEngine::open(home.path()).is_err());
    assert_eq!(snapshot(home.path()), before);
    assert!(
        !home
            .path()
            .join(DATA_DIRECTORY)
            .join(super::super::SESSIONS_DIRECTORY)
            .exists()
    );
    assert!(!home.path().join("backups").exists());
}

#[test]
fn missing_malformed_or_lowered_metadata_is_never_repaired() {
    for change in [
        "DROP TABLE database_compatibility",
        "DELETE FROM database_compatibility",
        "UPDATE database_compatibility SET min_compatible_host_version='0.25.1'",
        "UPDATE database_compatibility SET min_compatible_host_version='v0.25.2'",
        "UPDATE database_compatibility SET min_compatible_host_version='4294967296.0.0'",
        "DROP TABLE database_compatibility; CREATE TABLE database_compatibility(id, min_compatible_host_version); INSERT INTO database_compatibility VALUES(1,'0.25.2'),(2,'0.25.2')",
    ] {
        let home = tempfile::tempdir().unwrap();
        migrate(home.path(), "0.25.2", &manifest()).unwrap();
        preserve_before_fault(home.path());
        open(home.path()).execute_batch(change).unwrap();
        let before = snapshot(home.path());
        assert!(
            migrate(home.path(), "0.25.2", &manifest()).is_err(),
            "{change}"
        );
        assert_eq!(snapshot(home.path()), before, "{change}");
    }
}

#[test]
fn unknown_unversioned_database_and_newer_ledger_remain_unchanged() {
    let unknown = tempfile::tempdir().unwrap();
    fs::create_dir_all(unknown.path().join(DATA_DIRECTORY)).unwrap();
    open(unknown.path())
        .execute_batch("CREATE TABLE foreign_data(value); INSERT INTO foreign_data VALUES('keep')")
        .unwrap();
    let before = snapshot(unknown.path());
    assert!(migrate(unknown.path(), "0.25.2", &manifest()).is_err());
    assert_eq!(snapshot(unknown.path()), before);

    let newer = tempfile::tempdir().unwrap();
    migrate(newer.path(), "0.25.2", &manifest()).unwrap();
    preserve_before_fault(newer.path());
    open(newer.path())
        .execute("INSERT INTO schema_migrations VALUES('0.25.3',3)", [])
        .unwrap();
    let before = snapshot(newer.path());
    assert!(matches!(
        migrate(newer.path(), "0.25.2", &manifest()),
        Err(MigrationError::NewerDatabase)
    ));
    assert_eq!(snapshot(newer.path()), before);
}

#[test]
fn ledger_failure_rolls_back_business_minimum_and_version_together() {
    let home = tempfile::tempdir().unwrap();
    migrate(home.path(), BASELINE, &[v0_25_1::entry()]).unwrap();
    preserve_before_fault(home.path());
    // 注入记账失败，确保错误发生在兼容表创建之后，能观察整个版本事务的原子性。
    open(home.path()).execute_batch("CREATE TRIGGER reject_next_version BEFORE INSERT ON schema_migrations BEGIN SELECT RAISE(ABORT,'injected ledger failure'); END").unwrap();
    let mut next = v0_25_2::entry();
    next.apply = |connection| {
        connection.execute_batch("CREATE TABLE partial_upgrade(value)")?;
        Ok(())
    };
    let error = migrate(home.path(), "0.25.2", &[v0_25_1::entry(), next]).unwrap_err();
    let MigrationError::VersionFailed {
        backup: Some(backup),
        ..
    } = error
    else {
        panic!("expected verified backup and version failure")
    };
    let connection = open(home.path());
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(compatibility::read(&connection).unwrap().is_none());
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='partial_upgrade'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let copy = Connection::open_with_flags(
        backup.join("runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    backup::check_integrity(&copy).unwrap();
    assert_eq!(
        copy.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn migration_callback_cannot_create_or_change_compatibility_metadata() {
    for existing in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let target = if existing { "0.25.3" } else { "0.25.2" };
        let mut entries = vec![v0_25_1::entry()];
        if existing {
            entries.push(v0_25_2::entry());
        }
        migrate(home.path(), entries.last().unwrap().version, &entries).unwrap();
        entries.push(Migration {
            version: target,
            min_compatible_host_version: Some("0.25.2"),
            validate_source: v0_25_1::validate,
            apply: if existing {
                |c| {
                    c.execute(
                        "UPDATE database_compatibility SET min_compatible_host_version='0.25.1'",
                        [],
                    )?;
                    Ok(())
                }
            } else {
                |c| {
                    c.execute_batch("CREATE TABLE database_compatibility(id)")?;
                    Ok(())
                }
            },
            validate: v0_25_1::validate,
        });
        assert!(matches!(
            migrate(home.path(), target, &entries),
            Err(MigrationError::VersionFailed { .. })
        ));
        let current = compatibility::read(&open(home.path())).unwrap();
        assert_eq!(current, existing.then(|| parse_version("0.25.2").unwrap()));
    }
}

#[test]
fn compatible_later_release_preserves_floor_and_declining_manifest_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    let mut next = v0_25_2::entry();
    next.version = "0.25.3";
    let mut entries = vec![v0_25_1::entry(), v0_25_2::entry(), next];
    migrate(home.path(), "0.25.3", &entries).unwrap();
    assert_eq!(
        compatibility::read(&open(home.path())).unwrap(),
        Some(parse_version("0.25.2").unwrap())
    );
    entries[2].min_compatible_host_version = Some("0.25.1");
    let before = snapshot(home.path());
    assert!(matches!(
        migrate(home.path(), "0.25.3", &entries),
        Err(MigrationError::InvalidManifest)
    ));
    assert_eq!(snapshot(home.path()), before);
}

#[test]
fn nonempty_journal_and_changed_file_identity_are_rejected_without_recovery() {
    let home = tempfile::tempdir().unwrap();
    migrate(home.path(), "0.25.2", &manifest()).unwrap();
    preserve_before_fault(home.path());
    let path = path(home.path());
    let inspected = admission::ExistingDatabase::inspect(&path)
        .unwrap()
        .unwrap();
    let replacement = home.path().join("replacement");
    fs::copy(&path, &replacement).unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert!(matches!(
        inspected.check_unchanged(&path),
        Err(MigrationError::DatabaseChanged)
    ));
    fs::write(
        path.with_file_name(format!("{DATABASE_FILE}-journal")),
        [0; 512],
    )
    .unwrap();
    let before = snapshot(home.path());
    assert!(matches!(
        migrate(home.path(), "0.25.2", &manifest()),
        Err(MigrationError::UnsafeJournal)
    ));
    assert_eq!(snapshot(home.path()), before);
}
