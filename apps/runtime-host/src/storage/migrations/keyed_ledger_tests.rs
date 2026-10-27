//! 同发布版本追加项的账本与事务夹具；不注册生产 F02 迁移，不操作已有 Home。

use super::*;

fn home() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("f02-keyed-ledger-")
        .tempdir_in(root)
        .unwrap()
}

fn empty(_: &Connection) -> Result<()> {
    Ok(())
}
fn baseline(connection: &Connection) -> Result<()> {
    connection
        .execute_batch("CREATE TABLE fixture(id INTEGER PRIMARY KEY, value TEXT NOT NULL)")?;
    Ok(())
}
fn validate(connection: &Connection) -> Result<()> {
    connection.prepare("SELECT id,value FROM fixture LIMIT 0")?;
    Ok(())
}
fn append_z(connection: &Connection) -> Result<()> {
    connection.execute("INSERT INTO fixture VALUES(1,'first appended fact')", [])?;
    Ok(())
}
fn append_a(connection: &Connection) -> Result<()> {
    connection.execute("INSERT INTO fixture VALUES(2,'second appended fact')", [])?;
    Ok(())
}
fn entries() -> Vec<Migration> {
    vec![
        Migration {
            version: BASELINE,
            key: "release",
            min_compatible_host_version: None,
            rebuilds_referenced_tables: false,
            validate_source: empty,
            apply: baseline,
            validate,
        },
        Migration {
            version: "0.25.2",
            key: "release",
            min_compatible_host_version: Some(BASELINE),
            rebuilds_referenced_tables: false,
            validate_source: validate,
            apply: empty,
            validate,
        },
        Migration {
            version: "0.25.2",
            key: "fixture_z",
            min_compatible_host_version: Some("0.25.2"),
            rebuilds_referenced_tables: false,
            validate_source: validate,
            apply: append_z,
            validate,
        },
        Migration {
            version: "0.25.2",
            key: "fixture_a",
            min_compatible_host_version: Some("0.25.2"),
            rebuilds_referenced_tables: false,
            validate_source: validate,
            apply: append_a,
            validate,
        },
    ]
}

fn open(home: &Path) -> Connection {
    Connection::open(home.join("data/runtime.sqlite3")).unwrap()
}

#[test]
fn same_release_append_preserves_original_versions_times_and_is_idempotent() {
    let home = home();
    let entries = entries();
    migrate(home.path(), "0.25.2", &entries[..2]).unwrap();
    let source = open(home.path());
    assert!(!ledger_is_keyed(&source).unwrap());
    let before: Vec<(String, i64)> = source
        .prepare("SELECT version,applied_at_ms FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let report = migrate(home.path(), "0.25.2", &entries).unwrap();
    assert_eq!(report.applied, ["0.25.2", "0.25.2"]);
    assert!(ledger_is_keyed(&source).unwrap());
    let after: Vec<(String,i64)> = source.prepare("SELECT version,applied_at_ms FROM schema_migrations WHERE migration_key='release' ORDER BY version").unwrap()
        .query_map([], |row| Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(before, after);
    assert_eq!(
        source
            .query_row("SELECT COUNT(*) FROM fixture", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        source
            .query_row("SELECT value FROM fixture WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "first appended fact"
    );
    assert_eq!(
        compatibility::read(&source).unwrap().unwrap().to_string(),
        "0.25.2"
    );
    let saved = Connection::open_with_flags(
        report.backup.unwrap().join("runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert!(!ledger_is_keyed(&saved).unwrap());
    assert_eq!(
        saved
            .query_row("SELECT COUNT(*) FROM fixture", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let repeat = migrate(home.path(), "0.25.2", &entries).unwrap();
    assert!(repeat.applied.is_empty());
    assert!(repeat.backup.is_none());
}

#[test]
fn failed_append_rolls_back_business_ledger_shape_and_compatibility_together() {
    fn fail(connection: &Connection) -> Result<()> {
        connection.execute_batch("CREATE TABLE partial_fixture(id INTEGER); INSERT INTO fixture VALUES(9,'must roll back'); INSERT INTO missing VALUES(1)")?;
        Ok(())
    }
    let home = home();
    let mut entries = entries();
    migrate(home.path(), "0.25.2", &entries[..2]).unwrap();
    let source = open(home.path());
    let before = backup::evidence(&source).unwrap();
    entries[2].apply = fail;
    assert!(migrate(home.path(), "0.25.2", &entries).is_err());
    assert_eq!(backup::evidence(&source).unwrap(), before);
    assert!(!ledger_is_keyed(&source).unwrap());
    assert!(source.prepare("SELECT * FROM partial_fixture").is_err());
    assert_eq!(
        compatibility::read(&source).unwrap().unwrap().to_string(),
        BASELINE
    );
}

#[test]
fn append_order_comes_from_manifest_and_missing_middle_key_is_rejected() {
    let home = home();
    let entries = entries();
    migrate(home.path(), "0.25.2", &entries[..3]).unwrap();
    // fixture_z 的排序大于 fixture_a，但它是 manifest 中较早的追加项。
    let source = open(home.path());
    assert_eq!(
        completed_prefix(
            &source,
            &validate_manifest("0.25.2", &entries).unwrap(),
            &entries
        )
        .unwrap(),
        3
    );
    migrate(home.path(), "0.25.2", &entries).unwrap();
    source
        .execute(
            "DELETE FROM schema_migrations WHERE migration_key='fixture_z'",
            [],
        )
        .unwrap();
    let damaged = backup::evidence(&source).unwrap();
    assert!(matches!(
        migrate(home.path(), "0.25.2", &entries),
        Err(MigrationError::InvalidLedger)
    ));
    assert_eq!(backup::evidence(&source).unwrap(), damaged);
}

#[test]
fn duplicated_keys_and_append_without_its_release_are_invalid_manifests() {
    let mut entries = entries();
    entries[3].key = "fixture_z";
    assert!(matches!(
        validate_manifest("0.25.2", &entries),
        Err(MigrationError::InvalidManifest)
    ));
    entries[3].key = "fixture_a";
    entries[1].key = "not_a_release";
    assert!(matches!(
        validate_manifest("0.25.2", &entries),
        Err(MigrationError::InvalidManifest)
    ));
}

#[test]
fn an_unknown_same_version_key_is_rejected_before_any_write() {
    let home = home();
    let entries = entries();
    migrate(home.path(), "0.25.2", &entries).unwrap();
    let source = open(home.path());
    source
        .execute(
            "UPDATE schema_migrations SET migration_key='unknown' WHERE migration_key='fixture_a'",
            [],
        )
        .unwrap();
    let before = backup::evidence(&source).unwrap();
    assert!(matches!(
        migrate(home.path(), "0.25.2", &entries),
        Err(MigrationError::InvalidLedger)
    ));
    assert_eq!(backup::evidence(&source).unwrap(), before);
}
