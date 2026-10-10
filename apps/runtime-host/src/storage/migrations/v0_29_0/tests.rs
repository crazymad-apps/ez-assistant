use super::*;

#[test]
fn old_providers_are_preserved_and_new_policy_is_closed_by_default() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE sessions(session_id TEXT PRIMARY KEY, agent_shell_kind TEXT); CREATE TABLE inputs(input_kind TEXT, agent_shell_target TEXT); CREATE TABLE agent_shell_settings(default_agent_shell_kind TEXT); INSERT INTO sessions(session_id) VALUES('kept-session'); CREATE TABLE providers(provider_instance_id TEXT PRIMARY KEY, endpoint TEXT NOT NULL); INSERT INTO providers VALUES('kept','https://example.test/v1');").unwrap();
    apply(&connection).unwrap();
    let row: (String, bool, Option<String>, Option<String>) = connection.query_row("SELECT endpoint, server_history_enabled, server_history_support_override, server_history_key FROM providers WHERE provider_instance_id='kept'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(row, ("https://example.test/v1".into(), false, None, None));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT session_id, server_history_key FROM sessions",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            )
            .unwrap(),
        ("kept-session".into(), None)
    );
    assert!(
        connection
            .execute("UPDATE sessions SET server_history_key=''", [])
            .is_err()
    );
    assert!(
        connection
            .execute("UPDATE providers SET server_history_enabled=1", [])
            .is_err()
    );
    connection.execute("UPDATE providers SET server_history_enabled=1, server_history_key='new-binding', server_history_support_override='supported'",[]).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE providers SET server_history_support_override='unknown'",
                []
            )
            .is_err()
    );
}

#[test]
fn versioned_upgrade_preserves_provider_and_verifies_backup_and_old_host_rejection() {
    use super::super::{
        MigrationError, migrate, v0_25_1, v0_25_2, v0_25_3, v0_26_0, v0_27_0, v0_28_0,
    };
    use rusqlite::OpenFlags;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    std::fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f06-m1-migration-")
        .tempdir_in(root)
        .unwrap();
    let manifest = [
        v0_25_1::entry(),
        v0_25_2::entry(),
        v0_25_3::entry(),
        v0_26_0::entry(),
        v0_27_0::entry(),
        v0_28_0::entry(),
        v0_28_1::entry(),
        entry(),
    ];
    migrate(home.path(), "0.28.1", &manifest[..7]).unwrap();
    let path = home.path().join("data/runtime.sqlite3");
    let old = Connection::open(&path).unwrap();
    old.execute("INSERT INTO providers(provider_instance_id,display_name,provider_type,endpoint,protocol_preference,models_path,discovery_format,api_key) VALUES('kept','Kept','openai','https://api.openai.com/v1','responses','','openai','fixture-only')",[]).unwrap();
    old.execute("INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms) VALUES('kept-session','Kept Session','{\"parts\":[\"stable\"]}','[]','active',1,0,1,1)", []).unwrap();
    drop(old);
    let report = migrate(home.path(), "0.29.0", &manifest).unwrap();
    assert_eq!(report.applied, ["0.29.0"]);
    let backup = report.backup.unwrap();
    let saved = Connection::open_with_flags(
        backup.join("runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let upgraded = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for connection in [&saved, &upgraded] {
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT title FROM sessions WHERE session_id='kept-session'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "Kept Session"
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM providers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT endpoint FROM providers WHERE provider_instance_id='kept'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "https://api.openai.com/v1"
        );
    }
    assert_eq!(upgraded.query_row("SELECT server_history_enabled,server_history_support_override,server_history_key FROM providers",[],|row|Ok((row.get::<_,bool>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?))).unwrap(),(false,None,None));
    assert_eq!(
        upgraded
            .query_row(
                "SELECT server_history_key FROM sessions WHERE session_id='kept-session'",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap(),
        None
    );
    drop(upgraded);
    assert!(matches!(
        migrate(home.path(), "0.28.1", &manifest[..7]),
        Err(MigrationError::HostTooOld)
    ));
    if std::env::var_os("EZ_ASSISTANT_KEEP_TEST_FIXTURES").is_some() {
        println!("F06 verified migration backup: {}", backup.display());
        println!(
            "F06 verified migration Runtime Home: {}",
            home.keep().display()
        );
    }
}

#[test]
fn unix_shell_choices_preserve_values_and_message_only_constraint() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE sessions(session_id TEXT PRIMARY KEY, agent_shell_kind TEXT CHECK(agent_shell_kind IN ('posix_sh','cmd')));
        CREATE TABLE inputs(input_id TEXT PRIMARY KEY, input_kind TEXT, agent_shell_target TEXT CHECK(agent_shell_target IS NULL OR (input_kind='message' AND agent_shell_target IN ('posix_sh','cmd'))));
        CREATE TABLE agent_shell_settings(singleton_key INTEGER PRIMARY KEY, default_agent_shell_kind TEXT CHECK(default_agent_shell_kind IN ('posix_sh','cmd')));
        INSERT INTO sessions VALUES('bound','posix_sh'),('unbound',NULL);
        INSERT INTO inputs VALUES('message','message','posix_sh'),('command','command',NULL);
        INSERT INTO agent_shell_settings VALUES(1,'cmd');").unwrap();
    let transaction = connection.transaction().unwrap();
    shell_choices::apply(&transaction).unwrap();
    assert_eq!(
        transaction
            .query_row(
                "SELECT agent_shell_kind FROM sessions WHERE session_id='bound'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "posix_sh"
    );
    assert_eq!(
        transaction
            .query_row(
                "SELECT default_agent_shell_kind FROM agent_shell_settings",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "cmd"
    );
    for kind in ["bash", "zsh", "posix_sh"] {
        assert_eq!(
            transaction
                .execute(
                    "UPDATE sessions SET agent_shell_kind=?1 WHERE session_id='bound'",
                    [kind]
                )
                .unwrap(),
            1
        );
        assert_eq!(
            transaction
                .execute(
                    "UPDATE inputs SET agent_shell_target=?1 WHERE input_id='message'",
                    [kind]
                )
                .unwrap(),
            1
        );
        assert_eq!(
            transaction
                .execute(
                    "UPDATE agent_shell_settings SET default_agent_shell_kind=?1",
                    [kind]
                )
                .unwrap(),
            1
        );
        assert!(
            transaction
                .execute(
                    "UPDATE inputs SET agent_shell_target=?1 WHERE input_id='command'",
                    [kind]
                )
                .is_err()
        );
    }
    assert!(
        transaction
            .execute(
                "UPDATE sessions SET agent_shell_kind='arbitrary_program'",
                []
            )
            .is_err()
    );
    assert_eq!(
        transaction
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE agent_shell_kind IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    transaction.commit().unwrap();
    shell_choices::validate(&connection).unwrap();
}
