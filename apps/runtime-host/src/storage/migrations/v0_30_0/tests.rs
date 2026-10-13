use super::*;

#[test]
fn upgrade_preserves_history_billing_and_rejects_old_host() {
    use super::super::{
        MigrationError, migrate, v0_25_1, v0_25_2, v0_25_3, v0_26_0, v0_27_0, v0_28_0, v0_28_1,
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    std::fs::create_dir_all(&root).unwrap();
    // 无端口的本机 SQLite；仅本次新建目录、合成会话和账本。迁移控制器自行备份并核验。
    let home = tempfile::Builder::new()
        .prefix("f01-m1-migration-")
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
        v0_29_0::entry(),
        entry(),
    ];
    migrate(home.path(), "0.29.0", &manifest[..8]).unwrap();
    let path = home.path().join("data/runtime.sqlite3");
    let old = Connection::open(&path).unwrap();
    old.execute("INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms) VALUES('fixture','Kept','[]','[]','active',1,0,1,1)",[]).unwrap();
    old.execute("INSERT INTO model_request_records(session_id,owner_kind,owner_id,request_id,request_kind,input_tokens,output_tokens,total_tokens,completed_at_ms) VALUES('fixture','session','fixture','old-request','agent_turn',100,20,120,1)",[]).unwrap();
    old.execute("INSERT INTO providers(provider_instance_id,display_name,provider_type,endpoint,api_key,protocol_preference,models_path,discovery_format) VALUES('kept-provider','Kept provider','openai','https://example.test/v1','synthetic-key','responses','','openai')", []).unwrap();
    old.execute("INSERT INTO model_fixed_configs(provider_instance_id,model_id,context_window_tokens,max_output_tokens,mode_limits_json,capabilities_json,updated_at_ms) VALUES('kept-provider','kept-model',8192,1024,'{}','{}',1)", []).unwrap();
    drop(old);
    let report = migrate(home.path(), "0.30.0", &manifest).unwrap();
    assert_eq!(report.applied, ["0.30.0"]);
    let backup = report.backup.unwrap();
    let saved = Connection::open_with_flags(
        backup.join("runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let current = Connection::open(&path).unwrap();
    for connection in [&saved, &current] {
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM model_request_records", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT title FROM sessions WHERE session_id='fixture'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "Kept"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT total_tokens FROM model_request_records WHERE request_id='old-request'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            120
        );
    }
    assert_eq!(
        current
            .query_row(
                "SELECT context_management_mode FROM sessions WHERE session_id='fixture'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "local_summary"
    );
    assert!(
        current
            .execute("UPDATE sessions SET context_management_mode='mixed'", [])
            .is_err()
    );
    current.execute("INSERT INTO model_request_records(session_id,owner_kind,owner_id,request_id,request_kind,input_tokens,output_tokens,total_tokens,completed_at_ms) VALUES('fixture','session','fixture','compact-request','context_compaction',50,10,60,2)",[]).unwrap();
    assert_eq!(
        current
            .query_row("SELECT COUNT(*) FROM model_request_records", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(matches!(
        migrate(home.path(), "0.29.0", &manifest[..8]),
        Err(MigrationError::HostTooOld | MigrationError::NewerDatabase)
    ));
    for connection in [&saved, &current] {
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM providers", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM model_fixed_configs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT api_key FROM providers WHERE provider_instance_id='kept-provider'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "synthetic-key"
        );
    }
    assert!(current.query_row("SELECT server_history_key FROM providers WHERE provider_instance_id='kept-provider'", [], |row| row.get::<_, String>(0)).unwrap().len() >= 32);
    current.execute("UPDATE providers SET server_history_enabled=0,server_history_key='next-native-cycle' WHERE provider_instance_id='kept-provider'", []).unwrap();
    assert!(
        current
            .execute("UPDATE providers SET server_history_enabled=1,server_history_key=NULL WHERE provider_instance_id='kept-provider'", [])
            .is_err()
    );
    validate(&current).unwrap();
    eprintln!(
        "隔离迁移核验：数据库={}；备份={}；sessions=1→1，child_tasks=0→0，model_request_records=1→2（新增压缩费用 1 条）；备份回读核验通过，夹具结束后自动清理。",
        path.display(),
        backup.display()
    );
}
