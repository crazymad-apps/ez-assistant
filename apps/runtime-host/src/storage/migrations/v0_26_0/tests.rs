use super::*;
use crate::storage::migrations::{MigrationError, compatibility, migrate};
use rusqlite::{Connection, OpenFlags};

fn business_counts(connection: &Connection) -> (i64, i64, i64, i64) {
    (
        connection
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .unwrap(),
        connection
            .query_row("SELECT COUNT(*) FROM inputs", [], |row| row.get(0))
            .unwrap(),
    )
}

#[test]
fn additive_catalog_migration_preserves_rows_and_existing_minimum() {
    let home = tempfile::tempdir().unwrap();
    migrate(
        home.path(),
        "0.25.3",
        &[
            crate::storage::migrations::v0_25_1::entry(),
            crate::storage::migrations::v0_25_2::entry(),
            crate::storage::migrations::v0_25_3::entry(),
        ],
    )
    .unwrap();
    let path = home.path().join("data/runtime.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.execute("INSERT INTO providers(provider_instance_id,display_name,provider_type,endpoint,api_key,protocol_preference,models_path,discovery_format) VALUES ('provider-1','P','openai','https://example.test/v1','','chat_completions','/v1/models','openai')", []).unwrap();
    let before = business_counts(&connection);
    drop(connection);

    let report = migrate(
        home.path(),
        "0.26.0",
        &[
            crate::storage::migrations::v0_25_1::entry(),
            crate::storage::migrations::v0_25_2::entry(),
            crate::storage::migrations::v0_25_3::entry(),
            entry(),
        ],
    )
    .unwrap();
    assert_eq!(report.applied, ["0.26.0"]);
    let backup = report.backup.as_ref().unwrap().join("runtime.sqlite3");
    let backup = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(business_counts(&backup), before);
    drop(backup);
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(business_counts(&connection), before);
    let values: (Option<String>, Option<i64>, i64) = connection
        .query_row("SELECT model_catalog_json,model_catalog_refreshed_at_ms,model_catalog_connection_changed FROM providers WHERE provider_instance_id='provider-1'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))
        .unwrap();
    assert_eq!(values, (None, None, 0));
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version='0.26.0'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        compatibility::read(&connection)
            .unwrap()
            .unwrap()
            .to_string(),
        "0.25.3"
    );
    drop(connection);

    assert!(matches!(
        migrate(
            home.path(),
            "0.25.3",
            &[
                crate::storage::migrations::v0_25_1::entry(),
                crate::storage::migrations::v0_25_2::entry(),
                crate::storage::migrations::v0_25_3::entry(),
            ],
        ),
        Err(MigrationError::NewerDatabase)
    ));
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(business_counts(&connection), before);
}
