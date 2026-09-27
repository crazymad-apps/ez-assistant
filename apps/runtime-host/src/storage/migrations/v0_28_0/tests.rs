use super::*;

fn source() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    connection
        .execute_batch(
            "CREATE TABLE workspaces (
                workspace_id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                user_directory TEXT NOT NULL UNIQUE,
                additional_directories_json TEXT NOT NULL DEFAULT '[]',
                agent_directory TEXT NOT NULL UNIQUE,
                lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active','removed')),
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                removed_at_ms INTEGER
            );
            CREATE TABLE sessions(session_id TEXT PRIMARY KEY);
            CREATE TABLE session_resources(
                session_id TEXT PRIMARY KEY REFERENCES sessions(session_id) ON DELETE CASCADE,
                workspace_id TEXT REFERENCES workspaces(workspace_id),
                working_directory TEXT NOT NULL,
                additional_workspace_directories_json TEXT NOT NULL DEFAULT '[]',
                attachment_directory TEXT NOT NULL UNIQUE,
                private_directory TEXT NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL
            );
            INSERT INTO workspaces VALUES('w-old','Old','/tmp/old','[]','/tmp/agent-old','active',1,1,NULL);
            INSERT INTO workspaces VALUES('w-removed','Old','/tmp/removed','[\"/tmp/extra\"]','/tmp/agent-removed','removed',3,4,5);
            INSERT INTO sessions VALUES('s-old');
            INSERT INTO session_resources VALUES('s-old','w-old','/tmp/old','[]','/tmp/a','/tmp/p',1);",
        )
        .unwrap();
    connection
}

#[test]
fn rebuild_preserves_references_and_allows_multiple_null_user_directories() {
    let connection = source();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    apply(&connection).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();

    assert_eq!(
        connection
            .query_row("SELECT workspace_id FROM session_resources", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "w-old"
    );
    for id in ["w-null-1", "w-null-2"] {
        connection
            .execute(
                "INSERT INTO workspaces VALUES(?1,?1,NULL,'[]',?2,'active',2,2,NULL)",
                rusqlite::params![id, format!("/tmp/{id}")],
            )
            .unwrap();
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM workspaces WHERE user_directory IS NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert!(connection
        .execute(
            "INSERT INTO workspaces VALUES('w-bad','bad',NULL,'[\"/tmp/x\"]','/tmp/bad','active',3,3,NULL)",
            [],
        )
        .is_err());
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn rebuild_preserves_active_removed_and_same_label_rows_exactly() {
    type Row = (
        String,
        String,
        Option<String>,
        String,
        String,
        String,
        i64,
        i64,
        Option<i64>,
    );
    let connection = source();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    apply(&connection).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();

    let rows: Vec<Row> = connection
        .prepare(
            "SELECT workspace_id, label, user_directory, additional_directories_json,
                    agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms
             FROM workspaces ORDER BY workspace_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "w-old".to_owned(),
                "Old".to_owned(),
                Some("/tmp/old".to_owned()),
                "[]".to_owned(),
                "/tmp/agent-old".to_owned(),
                "active".to_owned(),
                1,
                1,
                None
            ),
            (
                "w-removed".to_owned(),
                "Old".to_owned(),
                Some("/tmp/removed".to_owned()),
                "[\"/tmp/extra\"]".to_owned(),
                "/tmp/agent-removed".to_owned(),
                "removed".to_owned(),
                3,
                4,
                Some(5)
            ),
        ]
    );
}

#[test]
fn plugin_tables_keep_source_and_active_package_atomic() {
    let connection = source();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    apply(&connection).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    for table in ["plugins", "plugin_operations"] {
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    connection
        .execute(
            "INSERT INTO plugins(name, source_kind, active_version, active_sha256,
            active_manifest_json, created_at_ms, updated_at_ms)
         VALUES('document-tools', 'upload', '1.0.0', 'abc', '{}', 1, 1)",
            [],
        )
        .unwrap();
    assert!(connection.execute(
        "INSERT INTO plugins(name, source_kind, source_center_id, created_at_ms, updated_at_ms)
         VALUES('invalid-source', 'center', NULL, 1, 1)", [],
    ).is_err());
    assert!(
        connection
            .execute(
                "UPDATE plugins SET active_manifest_json=NULL WHERE name='document-tools'",
                [],
            )
            .is_err()
    );
    connection
        .execute(
            "INSERT INTO plugin_operations(operation_key, kind, intent_json, phase,
            created_at_ms, updated_at_ms)
         VALUES('key-1', 'install', '{}', 'accepted', 1, 1)",
            [],
        )
        .unwrap();
    assert!(
        connection
            .execute(
                "INSERT INTO plugin_operations(operation_key, kind, intent_json, phase,
            created_at_ms, updated_at_ms)
         VALUES('key-2', 'install', '{}', 'unknown', 1, 1)",
                [],
            )
            .is_err()
    );
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM plugin_operations WHERE operation_key='key-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
