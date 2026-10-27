//! 只操作本场景新建的 SQLite 及受管文件；无网络连接或日常用户数据。

use std::fs;

use agent_types::{ConversationMessage, MessageId, PartId, TextPart, UserMessage, UserPart};

use super::super::{backup, compatibility, manifest, migrate, product_manifest};
use super::*;

fn fixture() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f02-upgrade-")
        .tempdir_in(root)
        .unwrap();
    migrate(home.path(), "0.32.1", &manifest()).unwrap();
    let connection = Connection::open(home.path().join("data/runtime.sqlite3")).unwrap();
    connection.execute_batch(
        "INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms)
         VALUES('old-session','retained','[]','[]','active',1,1,1,1);
         INSERT INTO inputs(input_id,session_id,user_message_id,state,accepted_at_ms)
         VALUES('old-input','old-session','old-user','committed',1);
         INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms,started_at_ms,finished_at_ms)
         VALUES('old-run','old-session','old-input',1,'failed',0,1,10,20);
         INSERT INTO run_message_refs(run_id,message_id,step) VALUES('old-run','old-user',NULL);"
    ).unwrap();
    let path = owner_path(home.path(), &owner(), 1).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let message = ConversationMessage::User(UserMessage {
        id: MessageId::new("old-user").unwrap(),
        origin: Default::default(),
        transcript_visibility: Default::default(),
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new("old-part").unwrap(),
            text: "retained input".into(),
        })],
    });
    fs::write(path, conversation::encode_messages(&[message]).unwrap()).unwrap();
    home
}

fn owner() -> ConversationOwner {
    ConversationOwner::MainSession {
        session_id: SessionId::new("old-session").unwrap(),
    }
}

fn database(home: &Path) -> Connection {
    Connection::open(home.join("data/runtime.sqlite3")).unwrap()
}

#[test]
fn migration_catalogue_is_batched_across_main_and_child_owners_without_execution_payloads() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE sessions(session_id TEXT PRIMARY KEY,body_generation INTEGER,message_count INTEGER);
        CREATE TABLE child_tasks(session_id TEXT,child_task_id TEXT PRIMARY KEY,body_generation INTEGER,message_count INTEGER,request_json TEXT);").unwrap();
    for index in 0..70 {
        connection
            .execute(
                "INSERT INTO sessions VALUES(?1,2,3)",
                [format!("session-{index:03}")],
            )
            .unwrap();
        for owner in ["session-000", "session-069"] {
            connection.execute("INSERT INTO child_tasks VALUES(?1,?2,4,5,'intentionally not an execution payload')",
                params![owner, format!("{owner}-child-{index:03}")]).unwrap();
        }
    }
    let changes = connection.total_changes();
    let mut after = None;
    let mut keys = std::collections::BTreeSet::new();
    let mut mains = 0;
    let mut children = 0;
    loop {
        let batch = owner_catalogue_batch(&connection, after.as_ref()).unwrap();
        assert!(batch.len() <= usize::try_from(CATALOGUE_BATCH_SIZE).unwrap());
        if batch.is_empty() {
            break;
        }
        for (owner, generation, count) in batch {
            match &owner {
                ConversationOwner::MainSession { session_id } => {
                    assert_eq!((generation, count), (2, 3));
                    assert_eq!(children, 0);
                    assert!(keys.insert(format!("main:{session_id}")));
                    mains += 1;
                }
                ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } => {
                    assert_eq!((generation, count), (4, 5));
                    assert_eq!(mains, 70);
                    assert!(keys.insert(format!("child:{session_id}:{child_task_id}")));
                    children += 1;
                }
            }
            after = Some(owner);
        }
    }
    assert_eq!((mains, children, keys.len()), (70, 140, 210));
    assert_eq!(
        connection.total_changes(),
        changes,
        "catalogue reads cannot mutate the source"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        70
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        140
    );
}

#[test]
fn terminal_columns_remain_required_in_old_sources_and_are_rejected_in_aligned_stores() {
    for (table, column) in [("runs", "error_code"), ("child_tasks", "final_message_id")] {
        let home = fixture();
        let connection = database(home.path());
        connection
            .execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column}"))
            .unwrap();
        let before = backup::evidence(&connection).unwrap();
        let bytes = fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap();
        drop(connection);
        assert!(migrate(home.path(), "0.32.1", &product_manifest()).is_err());
        let connection = database(home.path());
        assert_eq!(backup::evidence(&connection).unwrap(), before);
        assert!(!committed(&connection).unwrap());
        assert_eq!(
            fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap(),
            bytes
        );
    }
    let home = fixture();
    migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let mut engine = StorageEngine::open(home.path()).unwrap();
    let expected = engine.load_conversation_history(owner()).unwrap();
    drop(engine);
    for (table, columns) in [
        (
            "runs",
            &["error_code", "error_message", "finished_at_ms"][..],
        ),
        (
            "child_tasks",
            &[
                "final_message_id",
                "error_code",
                "error_message",
                "finished_at_ms",
            ][..],
        ),
    ] {
        for column in columns {
            let connection = database(home.path());
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
                        params![table, column],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            connection
                .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"))
                .unwrap();
            drop(connection);
            assert!(
                StorageEngine::open(home.path()).is_err(),
                "even an empty terminal column is not part of the target schema"
            );
            database(home.path())
                .execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column}"))
                .unwrap();
        }
    }
    let mut engine = StorageEngine::open(home.path()).unwrap();
    assert_eq!(engine.load_conversation_history(owner()).unwrap(), expected);
}

#[test]
fn late_child_column_drop_failure_rolls_back_run_columns_controls_pointers_and_ledger() {
    let (home, child_owner) = legacy_child_fixture();
    let connection = database(home.path());
    connection.execute_batch("UPDATE runs SET error_code='internal',error_message='parent terminal evidence' WHERE run_id='old-run';
        UPDATE child_tasks SET status='failed',error_code='internal',error_message='child terminal evidence' WHERE child_task_id='old-child';
        CREATE TRIGGER retain_child_terminal_column BEFORE INSERT ON child_tasks BEGIN SELECT NEW.final_message_id; END;").unwrap();
    let before = backup::evidence(&connection).unwrap();
    let parent_bytes = fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap();
    let child_bytes = fs::read(owner_path(home.path(), &child_owner, 1).unwrap()).unwrap();
    drop(connection);
    let MigrationError::VersionFailed {
        backup: Some(saved),
        ..
    } = migrate(home.path(), "0.32.1", &product_manifest()).unwrap_err()
    else {
        panic!("DDL failure must retain a verified independent source backup")
    };
    let saved_db = Connection::open_with_flags(
        saved.join("runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    backup::check_integrity(&saved_db).unwrap();
    assert_eq!(backup::evidence(&saved_db).unwrap(), before);
    let connection = database(home.path());
    assert_eq!(backup::evidence(&connection).unwrap(), before);
    assert!(!committed(&connection).unwrap());
    for checked in [&saved_db, &connection] {
        assert_eq!(checked.query_row("SELECT COUNT(*) FROM runs WHERE error_message='parent terminal evidence' AND finished_at_ms=20",[],|row|row.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(checked.query_row("SELECT COUNT(*) FROM child_tasks WHERE error_message='child terminal evidence' AND finished_at_ms=3",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    }
    for root in [home.path(), saved.as_path()] {
        assert_eq!(
            fs::read(owner_path(root, &owner(), 1).unwrap()).unwrap(),
            parent_bytes
        );
        assert_eq!(
            fs::read(owner_path(root, &child_owner, 1).unwrap()).unwrap(),
            child_bytes
        );
    }
    connection
        .execute_batch("DROP TRIGGER retain_child_terminal_column")
        .unwrap();
    drop(connection);
    migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let mut engine = StorageEngine::open(home.path()).unwrap();
    let history = engine.load_conversation_history(owner()).unwrap();
    assert_eq!(
        history.facts.runs[0].error.as_ref().unwrap().message,
        "parent terminal evidence"
    );
    assert_eq!(history.facts.runs[0].finished_at_ms, Some(20));
    let child = engine.load_conversation_history(child_owner).unwrap();
    assert_eq!(
        child.facts.child_tasks[0].error.as_ref().unwrap().message,
        "child terminal evidence"
    );
    assert_eq!(child.facts.child_tasks[0].finished_at_ms, Some(3));
    for table in ["runs", "child_tasks"] {
        assert_eq!(
            engine
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn upgrade_does_not_silently_drop_terminal_values_from_an_active_run_control() {
    let home = fixture();
    let connection = database(home.path());
    connection.execute_batch("UPDATE runs SET status='running',finished_at_ms=NULL,error_code='internal',error_message='unclassified control value'").unwrap();
    let before = backup::evidence(&connection).unwrap();
    drop(connection);
    assert!(migrate(home.path(), "0.32.1", &product_manifest()).is_err());
    let connection = database(home.path());
    assert_eq!(backup::evidence(&connection).unwrap(), before);
    assert!(!committed(&connection).unwrap());
}

#[test]
fn upgraded_usage_backfill_requires_proven_local_origin_and_keeps_unattributed_auxiliary_ledger() {
    use std::io::Write;
    let home = fixture();
    let connection = database(home.path());
    let usage = agent_types::TokenUsage {
        input_tokens: 100,
        output_tokens: 10,
        total_tokens: 110,
        cached_input_tokens: Some(40),
        reasoning_tokens: Some(5),
    };
    let model =
        agent_types::ModelIdentity::new(agent_types::ProviderId::new("fixture").unwrap(), "model");
    let local = agent_types::AssistantMessage {
        id: MessageId::new("proven-local").unwrap(),
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        model: model.clone(),
        parts: Vec::new(),
        finish_reason: agent_types::FinishReason::Stop,
        usage: Some(usage.clone()),
    };
    let mut unknown = local.clone();
    unknown.id = MessageId::new("unknown-legacy-fork").unwrap();
    let summary = agent_types::ContextSummaryMessage {
        id: MessageId::new("unknown-legacy-summary").unwrap(),
        retained_user_message_id: None,
        text: "legacy summary without a local request binding".into(),
        model: Some(model),
        usage: Some(usage),
        compacted_usage: None,
        usage_adjustment: None,
        programmatic_context: None,
    };
    let path = owner_path(home.path(), &owner(), 1).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            &conversation::encode_messages(&[
                ConversationMessage::Assistant(local),
                ConversationMessage::Assistant(unknown),
                ConversationMessage::ContextSummary(summary),
            ])
            .unwrap(),
        )
        .unwrap();
    connection.execute_batch(
        "UPDATE sessions SET message_count=4 WHERE session_id='old-session';
         INSERT INTO run_message_refs(run_id,message_id,step) VALUES('old-run','proven-local',1);
         INSERT INTO model_request_records(session_id,owner_kind,owner_id,request_id,run_id,request_kind,input_tokens,output_tokens,total_tokens,completed_at_ms)
         VALUES('old-session','session','old-session','old-auxiliary-request','old-run','context_summary',50,5,55,9);
         INSERT INTO session_usage(session_id,request_count,input_tokens_sum,output_tokens_sum,total_tokens_sum,backfilled,updated_at_ms)
         VALUES('old-session',1,50,5,55,0,20);"
    ).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM model_request_records", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    drop(connection);
    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = Connection::open_with_flags(
        report.backup.unwrap().join("runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    backup::check_integrity(&saved).unwrap();
    assert_eq!(saved.query_row("SELECT COUNT(*) FROM model_request_records WHERE request_id='old-auxiliary-request' AND total_tokens=55",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(
        saved
            .query_row("SELECT COUNT(*) FROM run_message_refs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let mut engine = StorageEngine::open(home.path()).unwrap();
    let session = SessionId::new("old-session").unwrap();
    assert_eq!(engine.get_session_usage(&session).unwrap().total_tokens, 55);
    engine
        .backfill_session_usage_scoped(Some(&session))
        .unwrap();
    let expected = engine.get_session_usage(&session).unwrap();
    assert_eq!(expected.request_count, 2);
    assert_eq!(expected.total_tokens, 165);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM model_request_records", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        2
    );
    assert_eq!(engine.connection.query_row("SELECT COUNT(*) FROM model_request_records WHERE request_id IN ('unknown-legacy-fork','unknown-legacy-summary')",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(engine.connection.query_row("SELECT total_tokens FROM model_request_records WHERE request_id='old-auxiliary-request'",[],|row|row.get::<_,i64>(0)).unwrap(),55);
    drop(engine);
    let mut reopened = StorageEngine::open(home.path()).unwrap();
    reopened
        .backfill_session_usage_scoped(Some(&session))
        .unwrap();
    assert_eq!(reopened.get_session_usage(&session).unwrap(), expected);
}

#[test]
fn usage_migration_removes_only_run_relation_and_keeps_exact_request_identity_and_values() {
    let home = fixture();
    let source = database(home.path());
    source.execute_batch(
        "INSERT INTO model_request_records(session_id,owner_kind,owner_id,request_id,run_id,request_kind,provider,model_id,input_tokens,output_tokens,total_tokens,cached_input_tokens,reasoning_tokens,completed_at_ms)
         VALUES('old-session','session','old-session','turn','old-run','agent_turn','fixture','model',100,20,120,40,5,12),
               ('old-session','session','old-session','summary','old-run','context_summary','fixture','small',30,10,40,NULL,NULL,13),
               ('old-session','child_task','old-child','child-turn','old-run','agent_turn','fixture','child-model',50,5,55,0,0,14);
         INSERT INTO session_usage(session_id,request_count,input_tokens_sum,output_tokens_sum,total_tokens_sum,backfilled,updated_at_ms)
         VALUES('old-session',2,130,30,160,1,14);"
    ).unwrap();
    let requests = "SELECT json_group_array(json_object('session',session_id,'kind',owner_kind,'owner',owner_id,'request',request_id,'request_kind',request_kind,'provider',provider,'model',model_id,'input',input_tokens,'output',output_tokens,'total',total_tokens,'cache',cached_input_tokens,'reasoning',reasoning_tokens,'at',completed_at_ms)) FROM (SELECT * FROM model_request_records ORDER BY session_id,owner_kind,owner_id,request_id)";
    let totals = "SELECT json_object('requests',request_count,'input',input_tokens_sum,'output',output_tokens_sum,'total',total_tokens_sum,'backfilled',backfilled,'at',updated_at_ms) FROM session_usage WHERE session_id='old-session'";
    let expected_requests: String = source.query_row(requests, [], |row| row.get(0)).unwrap();
    let expected_totals: String = source.query_row(totals, [], |row| row.get(0)).unwrap();
    assert_eq!(
        source
            .query_row("SELECT COUNT(*) FROM model_request_records", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        3
    );
    drop(source);

    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = Connection::open_with_flags(
        report.backup.unwrap().join("runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    backup::check_integrity(&saved).unwrap();
    assert_eq!(
        saved
            .query_row(requests, [], |row| row.get::<_, String>(0))
            .unwrap(),
        expected_requests
    );
    assert_eq!(
        saved
            .query_row(totals, [], |row| row.get::<_, String>(0))
            .unwrap(),
        expected_totals
    );
    assert_eq!(
        saved
            .query_row(
                "SELECT COUNT(*) FROM model_request_records WHERE run_id='old-run'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        3
    );
    let target = database(home.path());
    assert_eq!(
        target
            .query_row(requests, [], |row| row.get::<_, String>(0))
            .unwrap(),
        expected_requests
    );
    assert_eq!(
        target
            .query_row(totals, [], |row| row.get::<_, String>(0))
            .unwrap(),
        expected_totals
    );
    assert_eq!(
        target
            .query_row("SELECT COUNT(*) FROM model_request_records", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        3
    );
    assert!(
        target
            .prepare("SELECT run_id FROM model_request_records")
            .is_err()
    );
    assert_eq!(target.execute("INSERT OR IGNORE INTO model_request_records(session_id,owner_kind,owner_id,request_id,request_kind,input_tokens,output_tokens,total_tokens,completed_at_ms) VALUES('old-session','session','old-session','turn','agent_turn',999,999,1998,99)",[]).unwrap(),0);
    assert_eq!(
        target
            .query_row(requests, [], |row| row.get::<_, String>(0))
            .unwrap(),
        expected_requests
    );
    drop(target);
    migrate(home.path(), "0.32.1", &product_manifest()).unwrap();

    // 已对齐库不能借空列重新引入历史关联；正常准入拒绝开发中间布局。
    database(home.path())
        .execute_batch("ALTER TABLE model_request_records ADD COLUMN run_id TEXT")
        .unwrap();
    assert!(StorageEngine::open(home.path()).is_err());
}

#[test]
fn terminal_run_release_failure_restores_source_and_later_admission_rejects_reintroduced_terminal_rows()
 {
    let home = fixture();
    let connection = database(home.path());
    connection.execute_batch("CREATE TRIGGER fail_run_release BEFORE DELETE ON runs BEGIN SELECT RAISE(ABORT,'injected legacy Run release failure'); END;").unwrap();
    let before = backup::evidence(&connection).unwrap();
    let bytes = fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap();
    drop(connection);
    let error = migrate(home.path(), "0.32.1", &product_manifest()).unwrap_err();
    let MigrationError::VersionFailed {
        backup: Some(saved),
        ..
    } = error
    else {
        panic!("terminal release failure must preserve a verified source backup");
    };
    let original = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&original).unwrap();
    assert_eq!(backup::evidence(&original).unwrap(), before);
    assert_eq!(
        original
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        fs::read(owner_path(&saved, &owner(), 1).unwrap()).unwrap(),
        bytes
    );
    let connection = database(home.path());
    assert_eq!(backup::evidence(&connection).unwrap(), before);
    assert!(!committed(&connection).unwrap());
    assert_eq!(
        fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap(),
        bytes
    );
    connection
        .execute_batch("DROP TRIGGER fail_run_release")
        .unwrap();
    drop(connection);
    migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let mut engine = StorageEngine::open(home.path()).unwrap();
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let expected = engine.load_conversation_history(owner()).unwrap();
    assert_eq!(expected.facts.runs.len(), 1);
    assert_eq!(
        expected.facts.runs[0].status,
        Some(assistant_protocol::RunStatus::Failed)
    );
    engine.connection.execute("INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms,started_at_ms,shell_snapshot_json) VALUES('old-run','old-session','old-input',1,'failed',0,999,9999,'{}')", []).unwrap();
    assert!(
        engine
            .load_run_control(
                &SessionId::new("old-session").unwrap(),
                &assistant_protocol::RunId::new("old-run").unwrap()
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(engine.load_conversation_history(owner()).unwrap(), expected);
    drop(engine);
    assert!(StorageEngine::open(home.path()).is_err());
}

#[test]
fn completed_command_migration_keeps_only_identity_and_restores_with_its_queue_and_counter() {
    use assistant_protocol::{AgentVariant, IdempotencyKey, InputId, SessionCommand};
    use assistant_runtime::{
        StoredSessionCommand, StoredSessionCommandResult, StoredSessionCommandState,
    };
    use std::io::Write;
    let home = fixture();
    let session = SessionId::new("old-session").unwrap();
    let command = StoredSessionCommand {
        queue_order: 4,
        input_id: InputId::new("old-command").unwrap(),
        session_id: session.clone(),
        idempotency_key: Some(IdempotencyKey::new("old-command-key").unwrap()),
        user_message_id: MessageId::new("old-command-message").unwrap(),
        agent_variant: AgentVariant::Plan,
        command: SessionCommand::McpRefresh { server: None },
        result: Some(StoredSessionCommandResult::Mcp(
            assistant_protocol::McpRefreshControlResultSnapshot {
                outcome: assistant_protocol::McpRefreshOutcome::Success,
                servers: Vec::new(),
            },
        )),
        state: StoredSessionCommandState::Committed,
        accepted_at_ms: 51,
    };
    let connection = database(home.path());
    connection.execute("INSERT INTO inputs(input_id,session_id,idempotency_key,user_message_id,state,input_kind,command_json,command_result_json,accepted_at_ms,agent_variant,origin,priority_order) VALUES('old-command','old-session','old-command-key','old-command-message','committed','command',?1,?2,51,'plan','runtime',4)", params![serde_json::to_string(&command.command).unwrap(),serde_json::to_string(&command.result).unwrap()]).unwrap();
    let waiting_json = serde_json::to_string(&SessionCommand::McpRefresh { server: None }).unwrap();
    connection.execute("INSERT INTO inputs(input_id,session_id,idempotency_key,user_message_id,state,input_kind,command_json,accepted_at_ms,agent_variant,origin,priority_order) VALUES('waiting-command','old-session','waiting-key','waiting-command-message','queued','command',?1,70,'build','runtime',8)", [&waiting_json]).unwrap();
    connection
        .execute(
            "UPDATE sessions SET message_count=2 WHERE session_id='old-session'",
            [],
        )
        .unwrap();
    connection
        .execute("UPDATE sqlite_sequence SET seq=99 WHERE name='inputs'", [])
        .unwrap();
    let source_body = owner_path(home.path(), &owner(), 1).unwrap();
    let message = ConversationMessage::User(UserMessage {
        id: command.user_message_id.clone(),
        origin: agent_types::UserMessageOrigin::Runtime,
        transcript_visibility: Default::default(),
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new("old-command-part").unwrap(),
            text: "{RUNTIME_CONTROL_RESULT_V1} refreshed".into(),
        })],
    });
    fs::OpenOptions::new()
        .append(true)
        .open(&source_body)
        .unwrap()
        .write_all(&conversation::encode_messages(&[message]).unwrap())
        .unwrap();
    let source_bytes = fs::read(&source_body).unwrap();
    let evidence = backup::evidence(&connection).unwrap();
    drop(connection);
    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = report.backup.unwrap();
    let backup_db = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&backup_db).unwrap();
    assert_eq!(backup::evidence(&backup_db).unwrap(), evidence);
    assert_eq!(backup_db.query_row("SELECT COUNT(*) FROM inputs WHERE input_kind='command' AND command_json IS NOT NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
    assert_eq!(
        backup_db
            .query_row(
                "SELECT COUNT(*) FROM inputs WHERE command_result_json IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        fs::read(owner_path(&saved, &owner(), 1).unwrap()).unwrap(),
        source_bytes
    );
    {
        let root = home.path();
        let connection = database(root);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM inputs WHERE input_kind='command'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM inputs WHERE command_result_json IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM inputs WHERE input_id='old-command' AND command_json IS NULL AND user_message_id IS NULL AND accepted_at_ms IS NULL AND agent_variant IS NULL AND origin IS NULL AND priority_order IS NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        let waiting: (String, i64, i64) = connection.query_row("SELECT command_json,priority_order,accepted_at_ms FROM inputs WHERE input_id='waiting-command'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(waiting, (waiting_json.clone(), 8, 70));
        assert_eq!(
            connection
                .query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name='inputs'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            99
        );
        backup::check_integrity(&connection).unwrap();
        let mut engine = StorageEngine::open(root).unwrap();
        assert_eq!(
            engine
                .load_session_command(&session, &command.input_id)
                .unwrap(),
            Some(command.clone())
        );
    }
    // 在另一个新建隔离 Home 还原原数据库和唯一会话文件，再实际升级，不能只证明备份可打开。
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    let restored = tempfile::Builder::new()
        .prefix("f02-command-restore-")
        .tempdir_in(base)
        .unwrap();
    fs::create_dir_all(restored.path().join("data")).unwrap();
    fs::copy(
        saved.join("runtime.sqlite3"),
        restored.path().join("data/runtime.sqlite3"),
    )
    .unwrap();
    let restored_body = owner_path(restored.path(), &owner(), 1).unwrap();
    fs::create_dir_all(restored_body.parent().unwrap()).unwrap();
    fs::copy(owner_path(&saved, &owner(), 1).unwrap(), &restored_body).unwrap();
    let restored_db = database(restored.path());
    backup::check_integrity(&restored_db).unwrap();
    assert_eq!(backup::evidence(&restored_db).unwrap(), evidence);
    assert_eq!(fs::read(restored_body).unwrap(), source_bytes);
    drop(restored_db);
    migrate(restored.path(), "0.32.1", &product_manifest()).unwrap();
    let mut engine = StorageEngine::open(restored.path()).unwrap();
    assert_eq!(
        engine
            .load_session_command(&session, &command.input_id)
            .unwrap(),
        Some(command)
    );
    assert_eq!(engine.load_session_commands().unwrap().len(), 1);
    engine
        .accept_session_command(assistant_runtime::NewStoredSessionCommand {
            input_id: InputId::new("after-restoration").unwrap(),
            session_id: session,
            idempotency_key: None,
            user_message_id: MessageId::new("after-restoration-message").unwrap(),
            agent_variant: AgentVariant::Build,
            command: SessionCommand::McpRefresh { server: None },
            accepted_at_ms: 80,
        })
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT queue_order FROM inputs WHERE input_id='after-restoration'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        100
    );
}

#[test]
fn source_schema_requires_history_tables_and_current_schema_rejects_even_empty_copies() {
    for (current, table) in [
        (false, "run_message_refs"),
        (true, "run_message_refs"),
        (false, "message_feedback"),
        (true, "message_feedback"),
    ] {
        let home = fixture();
        if current {
            migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
        }
        let connection = database(home.path());
        if current {
            connection
                .execute_batch(&format!("CREATE TABLE {table}(unused TEXT)"))
                .unwrap();
        } else {
            connection
                .execute_batch(&format!("DROP TABLE {table}"))
                .unwrap();
        }
        let before = backup::evidence(&connection).unwrap();
        let generation = connection
            .query_row(
                "SELECT body_generation FROM sessions WHERE session_id='old-session'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let path = owner_path(home.path(), &owner(), u64::try_from(generation).unwrap()).unwrap();
        let bytes = fs::read(&path).unwrap();
        drop(connection);
        assert!(migrate(home.path(), "0.32.1", &product_manifest()).is_err());
        assert_eq!(backup::evidence(&database(home.path())).unwrap(), before);
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn legacy_message_feedback_moves_to_file_before_its_table_is_dropped() {
    let home = fixture();
    let connection = database(home.path());
    connection.execute("INSERT INTO message_feedback(session_id,message_id,feedback,changed_at_ms) VALUES('old-session','old-user','negative',42)", []).unwrap();
    drop(connection);
    let result = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = Connection::open(result.backup.unwrap().join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&saved).unwrap();
    assert_eq!(
        saved
            .query_row("SELECT COUNT(*) FROM message_feedback", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        saved
            .query_row(
                "SELECT message_id,feedback,changed_at_ms FROM message_feedback",
                [],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?
                ))
            )
            .unwrap(),
        ("old-user".into(), "negative".into(), 42)
    );
    drop(saved);
    let mut engine = StorageEngine::open(home.path()).unwrap();
    assert!(
        engine
            .legacy_message_feedback(&SessionId::new("old-session").unwrap())
            .is_err()
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='message_feedback'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let facts = engine.load_conversation_history(owner()).unwrap().facts;
    assert_eq!(facts.feedback.len(), 1);
    assert_eq!(facts.feedback[0].message_id.as_str(), "old-user");
    assert_eq!(
        facts.feedback[0].feedback,
        Some(assistant_protocol::MessageFeedback::Negative)
    );
    assert_eq!(facts.feedback[0].changed_at_ms, 42);
    engine
        .set_message_feedback(assistant_runtime::MessageFeedbackChange {
            session_id: SessionId::new("old-session").unwrap(),
            message_id: assistant_protocol::MessageId::new("old-user").unwrap(),
            feedback: None,
            changed_at_ms: 43,
        })
        .unwrap();
    drop(engine);
    let mut engine = StorageEngine::open(home.path()).unwrap();
    assert!(
        engine
            .load_message_feedback(&SessionId::new("old-session").unwrap())
            .unwrap()
            .is_empty()
    );
    let facts = engine.load_conversation_history(owner()).unwrap().facts;
    assert_eq!(facts.feedback.len(), 1);
    assert_eq!(facts.feedback[0].feedback, None);
    assert_eq!(facts.feedback[0].changed_at_ms, 43);
}

#[test]
fn legacy_nullable_step_is_only_read_in_the_verified_upgrade_context_then_moves_to_file() {
    let home = fixture();
    let connection = database(home.path());
    let saved = backup::create(
        &connection,
        home.path(),
        &home.path().join("data/runtime.sqlite3"),
        "0.32.1",
    )
    .unwrap();
    let original = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&original).unwrap();
    assert_eq!(
        original
            .query_row("SELECT COUNT(*) FROM run_message_refs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let mut legacy = StorageEngine::for_history_upgrade(home.path(), connection).unwrap();
    let runs = legacy
        .load_legacy_runs_scoped(Some(&SessionId::new("old-session").unwrap()))
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].message_ids[0].as_str(), "old-user");
    assert!(runs[0].message_steps.is_empty());
    drop(legacy);
    let mut current = StorageEngine::open(home.path()).unwrap();
    assert!(current.load_legacy_runs_scoped(None).is_err());
    assert_eq!(
        current
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='run_message_refs'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let history = current.load_conversation_history(owner()).unwrap();
    assert_eq!(
        history.facts.message_origins[0].message_id.as_str(),
        "old-user"
    );
    assert_eq!(history.facts.message_origins[0].step, None);
    assert_eq!(
        history.facts.message_origins[0]
            .run_id
            .as_ref()
            .unwrap()
            .as_str(),
        "old-run"
    );
}

#[test]
fn upgrade_binds_only_provable_original_materialization_identity() {
    for rewritten in [false, true] {
        let home = fixture();
        let connection = database(home.path());
        // 跟正式 Store 一样逐级拼接，避免 Windows 夹具保存混合分隔符的资源路径。
        let sessions = home.path().join("data").join("sessions");
        let session = crate::storage::tests::new_session("old-session", &sessions);
        let transaction = connection.unchecked_transaction().unwrap();
        StorageEngine::insert_session_resources(&transaction, &session).unwrap();
        transaction.commit().unwrap();
        connection.execute("UPDATE sessions SET materialization_key='legacy-materialization-key' WHERE session_id='old-session'", []).unwrap();
        if rewritten {
            let old = owner_path(home.path(), &owner(), 1).unwrap();
            fs::rename(old, owner_path(home.path(), &owner(), 2).unwrap()).unwrap();
            connection
                .execute(
                    "UPDATE sessions SET body_generation=2 WHERE session_id='old-session'",
                    [],
                )
                .unwrap();
        }
        drop(connection);
        let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
        let saved = report.backup.unwrap();
        let backup = Connection::open(saved.join("runtime.sqlite3")).unwrap();
        backup::check_integrity(&backup).unwrap();
        assert_eq!(
            backup
                .query_row("SELECT COUNT(*) FROM inputs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let connection = database(home.path());
        let bound: Option<String> = connection
            .query_row(
                "SELECT materialization_input_id FROM sessions WHERE session_id='old-session'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            bound.as_deref(),
            if rewritten { None } else { Some("old-input") }
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT materialization_key FROM sessions WHERE session_id='old-session'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "legacy-materialization-key"
        );
        drop(connection);
        let mut engine = StorageEngine::open(home.path()).unwrap();
        let key = assistant_protocol::IdempotencyKey::new("legacy-materialization-key").unwrap();
        if rewritten {
            assert_eq!(
                engine.get_session_materialization(&key).unwrap_err().kind(),
                assistant_runtime::StoreErrorKind::Conflict
            );
        } else {
            assert_eq!(
                engine
                    .get_session_materialization(&key)
                    .unwrap()
                    .unwrap()
                    .accepted
                    .input_id()
                    .as_str(),
                "old-input"
            );
        }
    }
}

#[test]
fn upgrade_moves_terminal_questions_to_files_and_preserves_pending_after_parent_control_release() {
    let home = fixture();
    let connection = database(home.path());
    connection
        .execute(
            "UPDATE runs SET status='completed' WHERE run_id='old-run'",
            [],
        )
        .unwrap();
    let mut pending = assistant_protocol::FeedbackRequestSnapshot {
        request_id: "old-question-pending".into(),
        session_id: SessionId::new("old-session").unwrap(),
        run_id: assistant_protocol::RunId::new("old-run").unwrap(),
        tool_call_id: assistant_protocol::ToolCallId::new("old-question-call").unwrap(),
        mode: assistant_protocol::FeedbackMode::EndTurn,
        questions: vec![assistant_protocol::FeedbackQuestion {
            question: "retained question".into(),
            header: None,
            options: Vec::new(),
            multiple: false,
        }],
        status: assistant_protocol::FeedbackStatus::Pending,
        answers: None,
    };
    let mut answered = pending.clone();
    answered.request_id = "old-question-answered".into();
    answered.status = assistant_protocol::FeedbackStatus::Answered;
    answered.answers = Some(vec![assistant_protocol::FeedbackAnswer {
        selected_options: Vec::new(),
        text: "retained answer".into(),
    }]);
    for question in [&pending, &answered] {
        connection.execute("INSERT INTO session_feedback(request_id,session_id,run_id,snapshot_json) VALUES(?1,'old-session','old-run',?2)",
            params![question.request_id,serde_json::to_string(question).unwrap()]).unwrap();
    }
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_feedback", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(connection);
    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = report.backup.unwrap();
    let backup = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&backup).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM session_feedback", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let saved_answer: String = backup
        .query_row(
            "SELECT snapshot_json FROM session_feedback WHERE request_id=?1",
            [&answered.request_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<assistant_protocol::FeedbackRequestSnapshot>(&saved_answer).unwrap(),
        answered
    );
    let document =
        conversation::read_document(&owner_path(home.path(), &owner(), 2).unwrap(), &owner(), 2)
            .unwrap();
    assert_eq!(document.history.unwrap().questions, vec![answered.clone()]);
    assert_eq!(document.conversation.messages.len(), 1);
    let connection = database(home.path());
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_feedback", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id='old-run'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_feedback", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(connection);
    let mut engine = StorageEngine::open(home.path()).unwrap();
    let requests = engine
        .load_feedback(&SessionId::new("old-session").unwrap())
        .unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.contains(&answered));
    assert!(requests.contains(&pending));
    pending.status = assistant_protocol::FeedbackStatus::Cancelled;
    engine
        .mutate_feedback(assistant_runtime::FeedbackMutation {
            request: pending.clone(),
            expected_status: Some(assistant_protocol::FeedbackStatus::Pending),
        })
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM session_feedback", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        engine
            .load_feedback(&SessionId::new("old-session").unwrap())
            .unwrap()
            .contains(&pending)
    );
}

fn legacy_child_fixture() -> (tempfile::TempDir, ConversationOwner) {
    let home = fixture();
    let connection = database(home.path());
    // 此场景只有 User 原文及已知 SQL child 关系，不能凭关系补造父工具消息。
    connection.execute("INSERT INTO child_tasks(child_task_id,session_id,parent_run_id,parent_tool_call_id,title,system_prompt_json,agent_variant,status,cancel_requested,body_generation,message_count,created_at_ms,finished_at_ms) VALUES('old-child','old-session','old-run','missing-call','retained child','[]','build','interrupted',0,1,0,2,3)", []).unwrap();
    drop(connection);
    let child_owner = ConversationOwner::ChildTask {
        session_id: SessionId::new("old-session").unwrap(),
        child_task_id: ChildTaskId::new("old-child").unwrap(),
    };
    let child_source = owner_path(home.path(), &child_owner, 1).unwrap();
    fs::create_dir_all(child_source.parent().unwrap()).unwrap();
    fs::write(&child_source, b"").unwrap();
    (home, child_owner)
}

#[test]
fn child_release_failure_rolls_back_pointers_schema_and_ledger_without_losing_source_or_backup() {
    let (home, child_owner) = legacy_child_fixture();
    let connection = database(home.path());
    connection.execute("INSERT INTO message_feedback(session_id,message_id,feedback,changed_at_ms) VALUES('old-session','old-user','positive',41)", []).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_old_child_release BEFORE DELETE ON child_tasks BEGIN SELECT RAISE(ABORT,'injected old child release failure'); END;").unwrap();
    let before = backup::evidence(&connection).unwrap();
    let main_source = fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap();
    let child_source = fs::read(owner_path(home.path(), &child_owner, 1).unwrap()).unwrap();
    drop(connection);
    let error = migrate(home.path(), "0.32.1", &product_manifest()).unwrap_err();
    let MigrationError::VersionFailed {
        backup: Some(saved),
        ..
    } = error
    else {
        panic!("failed child release requires a verified backup");
    };
    let backup_db = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&backup_db).unwrap();
    assert_eq!(backup::evidence(&backup_db).unwrap(), before);
    assert_eq!(
        backup_db
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        fs::read(owner_path(&saved, &owner(), 1).unwrap()).unwrap(),
        main_source
    );
    assert_eq!(
        fs::read(owner_path(&saved, &child_owner, 1).unwrap()).unwrap(),
        child_source
    );
    let connection = database(home.path());
    assert_eq!(backup::evidence(&connection).unwrap(), before);
    // 故障在 DROP 历史表之后，事务必须恢复表及原评价，不能只回滚文件指针。
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM message_feedback WHERE session_id='old-session' AND message_id='old-user' AND feedback='positive' AND changed_at_ms=41", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert!(!committed(&connection).unwrap());
    assert!(!super::super::ledger_is_keyed(&connection).unwrap());
    assert_eq!(
        connection
            .query_row(
                "SELECT body_generation FROM sessions WHERE session_id='old-session'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM child_tasks WHERE child_task_id='old-child' AND status='interrupted' AND body_generation=1",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(
        fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap(),
        main_source
    );
    assert_eq!(
        fs::read(owner_path(home.path(), &child_owner, 1).unwrap()).unwrap(),
        child_source
    );
    connection
        .execute_batch("DROP TRIGGER fail_old_child_release")
        .unwrap();
    drop(connection);
    migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    assert_eq!(
        database(home.path())
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut engine = StorageEngine::open(home.path()).unwrap();
    let history = engine
        .load_child_history_catalog(&SessionId::new("old-session").unwrap())
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].child_task_id.as_str(), "old-child");
    assert_eq!(history[0].finished_at_ms, Some(3));
    assert!(history[0].body_generation > 1);
}

#[test]
fn upgrade_keeps_known_child_directory_when_the_parent_call_is_absent_from_legacy_body() {
    let (home, child_owner) = legacy_child_fixture();
    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = report.backup.unwrap();
    let backup_connection = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    assert_eq!(
        backup_connection
            .query_row(
                "SELECT COUNT(*) FROM child_tasks WHERE child_task_id='old-child'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        fs::read(owner_path(&saved, &child_owner, 1).unwrap()).unwrap(),
        b""
    );
    let document =
        conversation::read_document(&owner_path(home.path(), &owner(), 2).unwrap(), &owner(), 2)
            .unwrap();
    assert_eq!(document.conversation.messages.len(), 1);
    let history = document.history.unwrap();
    assert_eq!(history.child_tasks.len(), 1);
    let child = &history.child_tasks[0];
    assert_eq!(child.child_task_id.as_str(), "old-child");
    assert_eq!(child.parent_run_id.as_str(), "old-run");
    assert_eq!(child.parent_tool_call_id.as_str(), "missing-call");
    assert_eq!(child.body_generation, 2);
    assert_eq!(child.started_at_ms, None);
    assert_eq!(child.finished_at_ms, Some(3));
    let child_document = conversation::read_document(
        &owner_path(home.path(), &child_owner, 2).unwrap(),
        &child_owner,
        2,
    )
    .unwrap();
    assert!(child_document.conversation.messages.is_empty());
    assert_eq!(
        child_document.history.unwrap().child_tasks,
        history.child_tasks
    );
    let connection = database(home.path());
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM child_tasks WHERE child_task_id='old-child'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(connection);
    // 在另一新建 Home 配套恢复 SQLite 和受管文件，再次升级，证明备份不是只可读的孤立 DB。
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    let restored = tempfile::Builder::new()
        .prefix("f02-child-restore-")
        .tempdir_in(base)
        .unwrap();
    let mut entries = std::collections::BTreeMap::new();
    files::inventory(&saved, Path::new("data"), &mut entries).unwrap();
    for (relative, entry) in entries {
        files::copy_entry(
            &saved.join(&relative),
            &restored.path().join(relative),
            &entry,
        )
        .unwrap();
    }
    let backup_db = saved.join("runtime.sqlite3");
    files::copy_entry(
        &backup_db,
        &restored.path().join("data/runtime.sqlite3"),
        &files::inspect(&backup_db).unwrap(),
    )
    .unwrap();
    let restored_db = database(restored.path());
    backup::check_integrity(&restored_db).unwrap();
    assert_eq!(
        backup::evidence(&restored_db).unwrap(),
        backup::evidence(&backup_connection).unwrap()
    );
    assert_eq!(
        restored_db
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        fs::read(owner_path(restored.path(), &child_owner, 1).unwrap()).unwrap(),
        b""
    );
    drop(restored_db);
    migrate(restored.path(), "0.32.1", &product_manifest()).unwrap();
    assert_eq!(
        database(restored.path())
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut restored_engine = StorageEngine::open(restored.path()).unwrap();
    assert_eq!(
        restored_engine
            .load_child_history_catalog(&SessionId::new("old-session").unwrap())
            .unwrap(),
        history.child_tasks
    );
    let mut engine = StorageEngine::open(home.path()).unwrap();
    assert_eq!(
        engine
            .load_child_history_catalog(&SessionId::new("old-session").unwrap())
            .unwrap(),
        history.child_tasks
    );
    assert!(
        engine
            .load_child_conversation(
                &SessionId::new("old-session").unwrap(),
                &ChildTaskId::new("old-child").unwrap()
            )
            .unwrap()
            .messages
            .is_empty()
    );
}

#[test]
fn upgrade_preserves_the_sql_input_queue_and_never_publishes_queued_payload_as_history() {
    use assistant_runtime::{
        SkillActivationOwner, SkillActivationTrigger, SkillName, StoredSkillActivation,
    };
    let home = fixture();
    let mut connection = database(home.path());
    let mut queued = UserMessage {
        id: MessageId::new("queued-user").unwrap(),
        origin: Default::default(),
        transcript_visibility: Default::default(),
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new("queued-part").unwrap(),
            text: "must remain queued".into(),
        })],
    };
    queued.parts.push(UserPart::InternalContext(
        agent_types::InternalContextPart::new(
            PartId::new("queued-skill-part").unwrap(),
            "queued-skill-boundary",
            "skill_activation",
            "SKILL_ACTIVATION_V1\nQueued selection",
        )
        .unwrap(),
    ));
    let queued_json = serde_json::to_string(&queued).unwrap();
    connection.execute("INSERT INTO inputs(input_id,session_id,user_message_id,state,queued_message_json,accepted_at_ms,priority_order) VALUES('queued-input','old-session','queued-user','queued',?1,42,3)", [&queued_json]).unwrap();
    connection.execute("INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms) VALUES('queued-run','old-session','queued-input',1,'accepted',0,42)", []).unwrap();
    connection.execute("INSERT INTO mcp_input_selections(selection_id,session_id,input_id,message_id,server_key,display_name,created_at_ms) VALUES('queued-mcp','old-session','queued-input','queued-user','queued-server','Queued server',42)", []).unwrap();
    let digest = format!("sha256-v1:{}", "a".repeat(64));
    let queued_skill = StoredSkillActivation {
        activation_id: "queued-skill".into(),
        session_id: SessionId::new("old-session").unwrap(),
        owner: SkillActivationOwner::Session(SessionId::new("old-session").unwrap()),
        run_id: Some(assistant_protocol::RunId::new("queued-run").unwrap()),
        input_id: Some(assistant_protocol::InputId::new("queued-input").unwrap()),
        message_id: MessageId::new("queued-user").unwrap(),
        name: SkillName::parse("queued-skill").unwrap(),
        catalog_revision: digest.clone(),
        definition_digest: digest.clone(),
        trigger: SkillActivationTrigger::User,
        created_at_ms: 42,
    };
    let historical_skill = StoredSkillActivation {
        activation_id: "old-skill".into(),
        run_id: Some(assistant_protocol::RunId::new("old-run").unwrap()),
        input_id: Some(assistant_protocol::InputId::new("old-input").unwrap()),
        message_id: MessageId::new("old-user").unwrap(),
        created_at_ms: 1,
        ..queued_skill.clone()
    };
    let transaction = connection.transaction().unwrap();
    for activation in [&queued_skill, &historical_skill] {
        crate::storage::skill::insert_skill_activation(&transaction, activation).unwrap();
        transaction
            .execute(
                "UPDATE inputs SET skill_activation_json=?1 WHERE input_id=?2",
                params![
                    serde_json::to_string(activation).unwrap(),
                    activation.input_id.as_ref().unwrap().as_str()
                ],
            )
            .unwrap();
    }
    transaction.execute("INSERT INTO mcp_input_selections(selection_id,session_id,input_id,message_id,server_key,display_name,created_at_ms) VALUES('old-mcp','old-session','old-input','old-user','old-server','Old server',1)", []).unwrap();
    transaction.commit().unwrap();
    connection.execute_batch("CREATE TRIGGER fail_selection_migration BEFORE DELETE ON skill_activations BEGIN SELECT RAISE(ABORT,'injected selection migration failure'); END;").unwrap();
    let before = backup::evidence(&connection).unwrap();
    drop(connection);
    let error = migrate(home.path(), "0.32.1", &product_manifest()).unwrap_err();
    let MigrationError::VersionFailed {
        backup: Some(saved),
        ..
    } = error
    else {
        panic!("selection migration failure must retain its verified source backup");
    };
    let saved = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&saved).unwrap();
    assert_eq!(backup::evidence(&saved).unwrap(), before);
    let connection = database(home.path());
    assert_eq!(backup::evidence(&connection).unwrap(), before);
    connection
        .execute_batch("DROP TRIGGER fail_selection_migration")
        .unwrap();
    drop(connection);
    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = Connection::open(report.backup.unwrap().join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&saved).unwrap();
    for table in ["skill_activations", "mcp_input_selections"] {
        assert_eq!(
            saved
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    let connection = database(home.path());
    for table in ["skill_activations", "mcp_input_selections"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM inputs WHERE input_id='old-input' AND skill_activation_json IS NOT NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM inputs WHERE input_id='old-input' AND user_message_id IS NULL AND accepted_at_ms IS NULL AND agent_variant IS NULL AND origin IS NULL AND channel_source_json IS NULL AND cross_session_json IS NULL AND goal_id IS NULL",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    let queue: (String, String, i64) = connection.query_row("SELECT state,queued_message_json,priority_order FROM inputs WHERE input_id='queued-input'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(queue, ("queued".into(), queued_json, 3));
    let selection: (String, String, String, i64) = connection.query_row(
        "SELECT input_id,message_id,server_key,created_at_ms FROM mcp_input_selections WHERE selection_id='queued-mcp'", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(
        selection,
        (
            "queued-input".into(),
            "queued-user".into(),
            "queued-server".into(),
            42
        )
    );
    let activation: (Option<String>, String, String, String, String, String, String, i64) = connection.query_row(
        "SELECT run_id,input_id,message_id,name,catalog_revision,definition_digest,trigger,created_at_ms FROM skill_activations WHERE activation_id='queued-skill'", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))).unwrap();
    assert_eq!(
        activation,
        (
            None,
            "queued-input".into(),
            "queued-user".into(),
            "queued-skill".into(),
            digest.clone(),
            digest,
            "user".into(),
            42
        )
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id='queued-run' AND status='accepted'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let document =
        conversation::read_document(&owner_path(home.path(), &owner(), 2).unwrap(), &owner(), 2)
            .unwrap();
    assert_eq!(document.conversation.messages.len(), 1);
    let history = document.history.unwrap();
    assert_eq!(history.inputs.len(), 1);
    assert_eq!(history.inputs[0].input_id.as_str(), "old-input");
    assert_eq!(history.mcp_selections.len(), 1);
    assert_eq!(history.mcp_selections[0].selection_id, "old-mcp");
    assert_eq!(history.skill_activations, vec![historical_skill.clone()]);
    assert!(
        history
            .runs
            .iter()
            .all(|run| run.run_id.as_str() != "queued-run")
    );
    drop(connection);
    let mut engine = StorageEngine::open(home.path()).unwrap();
    let loaded = engine
        .prepare_session_execution(&SessionId::new("old-session").unwrap())
        .unwrap()
        .state;
    assert_eq!(
        loaded.skill_activations,
        vec![historical_skill, {
            let mut queued_skill = queued_skill;
            queued_skill.run_id = None;
            queued_skill
        }]
    );
    assert_eq!(loaded.mcp_input_selections.len(), 2);
}

#[test]
fn same_version_upgrade_prepares_files_then_switches_once_with_matching_backup() {
    let home = fixture();
    let before = backup::evidence(&database(home.path())).unwrap();
    let old_bytes = fs::read(owner_path(home.path(), &owner(), 1).unwrap()).unwrap();
    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    assert_eq!(report.applied, ["0.32.1", "0.32.1", "0.32.1"]);
    let saved = report.backup.unwrap();
    let backup_connection = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&backup_connection).unwrap();
    assert_eq!(backup::evidence(&backup_connection).unwrap(), before);
    assert_eq!(
        fs::read(owner_path(&saved, &owner(), 1).unwrap()).unwrap(),
        old_bytes
    );
    assert!(saved.join("manifest.json").is_file());

    let connection = database(home.path());
    let generation: i64 = connection
        .query_row(
            "SELECT body_generation FROM sessions WHERE session_id='old-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(generation, 2);
    assert!(committed(&connection).unwrap());
    assert_eq!(
        compatibility::read(&connection)
            .unwrap()
            .unwrap()
            .to_string(),
        "0.32.1"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version='0.32.1'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    let document =
        conversation::read_document(&owner_path(home.path(), &owner(), 2).unwrap(), &owner(), 2)
            .unwrap();
    assert_eq!(document.conversation.messages.len(), 1);
    let history = document.history.unwrap();
    assert_eq!(history.runs[0].run_id.as_str(), "old-run");
    assert_eq!(history.runs[0].started_at_ms, Some(10));
    assert_eq!(history.runs[0].finished_at_ms, Some(20));
    assert_eq!(history.message_origins[0].step, None);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='run_message_refs'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(connection);
    let repeated = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    assert!(repeated.applied.is_empty());
    assert!(repeated.backup.is_none());
}

#[test]
fn upgrade_reconciles_only_terminal_queued_input_before_streaming_its_history() {
    let home = fixture();
    let source = owner_path(home.path(), &owner(), 1).unwrap();
    let decoded =
        conversation::decode(std::io::BufReader::new(fs::File::open(&source).unwrap())).unwrap();
    let ConversationMessage::User(consumed) = &decoded.messages[0] else {
        unreachable!()
    };
    let consumed_json = serde_json::to_string(consumed).unwrap();
    let mut waiting = consumed.clone();
    waiting.id = MessageId::new("waiting-user").unwrap();
    waiting.parts = vec![UserPart::Text(TextPart {
        id: PartId::new("waiting-text").unwrap(),
        text: "unconsumed".repeat(8_192),
    })];
    let waiting_json = serde_json::to_string(&waiting).unwrap();
    let connection = database(home.path());
    assert_eq!(
        connection
            .execute("DELETE FROM run_message_refs WHERE run_id='old-run'", [])
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE sessions SET message_count=0 WHERE session_id='old-session'",
                []
            )
            .unwrap(),
        1
    );
    assert_eq!(connection.execute("UPDATE inputs SET state='queued',queued_message_json=?1,priority_order=0 WHERE input_id='old-input'", [&consumed_json]).unwrap(), 1);
    connection.execute("INSERT INTO inputs(input_id,session_id,user_message_id,state,queued_message_json,accepted_at_ms,priority_order) VALUES('waiting-input','old-session','waiting-user','queued',?1,42,1)", [&waiting_json]).unwrap();
    connection.execute("INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms) VALUES('waiting-run','old-session','waiting-input',1,'accepted',0,42)", []).unwrap();
    fs::write(&source, b"").unwrap();
    drop(connection);

    let report = migrate(home.path(), "0.32.1", &product_manifest()).unwrap();
    let saved = report.backup.unwrap();
    // 配套备份保持恢复前原件；恢复完成后的源再整体转换，不伪装成 SQL 事务可撤销的恢复。
    assert!(
        fs::read(owner_path(&saved, &owner(), 1).unwrap())
            .unwrap()
            .is_empty()
    );
    let backup_connection = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    assert_eq!(
        backup_connection
            .query_row(
                "SELECT queued_message_json FROM inputs WHERE input_id='old-input'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        consumed_json
    );
    let connection = database(home.path());
    let queue = |id: &str| {
        connection
            .query_row(
                "SELECT state,queued_message_json,priority_order FROM inputs WHERE input_id=?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .unwrap()
    };
    assert_eq!(queue("old-input"), ("committed".into(), None, None));
    assert_eq!(
        queue("waiting-input"),
        ("queued".into(), Some(waiting_json), Some(1))
    );
    let document =
        conversation::read_document(&owner_path(home.path(), &owner(), 2).unwrap(), &owner(), 2)
            .unwrap();
    assert_eq!(document.conversation.messages, decoded.messages);
    let facts = document.history.unwrap();
    assert_eq!(facts.runs.len(), 1);
    assert_eq!(facts.inputs.len(), 1);
    assert_eq!(facts.message_origins.len(), 1);
    assert_eq!(
        facts.runs[0].status,
        Some(assistant_protocol::RunStatus::Failed)
    );
    assert_eq!(facts.inputs[0].input_id.as_str(), "old-input");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn one_bad_owner_rejects_the_whole_upgrade_without_publishing_prepared_files() {
    let home = fixture();
    let connection = database(home.path());
    connection.execute("INSERT INTO sessions(session_id,title,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms) VALUES('zz-missing','bad','[]','[]','active',1,1,2,2)", []).unwrap();
    let before = backup::evidence(&connection).unwrap();
    drop(connection);
    let error = migrate(home.path(), "0.32.1", &product_manifest()).unwrap_err();
    let super::super::MigrationError::VersionFailed {
        backup: Some(saved),
        ..
    } = error
    else {
        panic!("verified backup and failed version required");
    };
    assert!(saved.join("manifest.json").is_file());
    let connection = database(home.path());
    assert_eq!(backup::evidence(&connection).unwrap(), before);
    assert!(!committed(&connection).unwrap());
    assert_eq!(
        connection
            .query_row("SELECT MIN(body_generation) FROM sessions", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    assert!(
        conversation::read_document(&owner_path(home.path(), &owner(), 1).unwrap(), &owner(), 1)
            .unwrap()
            .history
            .is_none()
    );
}

#[test]
fn upgrade_rejects_committed_input_without_body_instead_of_silently_dropping_known_history() {
    let home = fixture();
    let connection = database(home.path());
    connection.execute("INSERT INTO inputs(input_id,session_id,user_message_id,state,accepted_at_ms) VALUES('orphan-input','old-session','missing-user','committed',5)", []).unwrap();
    let before = backup::evidence(&connection).unwrap();
    let source = owner_path(home.path(), &owner(), 1).unwrap();
    let source_bytes = fs::read(&source).unwrap();
    drop(connection);
    let error = migrate(home.path(), "0.32.1", &product_manifest()).unwrap_err();
    let MigrationError::VersionFailed {
        backup: Some(saved),
        ..
    } = error
    else {
        panic!("invalid history must fail with a verified backup");
    };
    let saved_connection = Connection::open(saved.join("runtime.sqlite3")).unwrap();
    backup::check_integrity(&saved_connection).unwrap();
    assert_eq!(backup::evidence(&saved_connection).unwrap(), before);
    assert_eq!(
        fs::read(owner_path(&saved, &owner(), 1).unwrap()).unwrap(),
        source_bytes
    );
    let connection = database(home.path());
    assert_eq!(backup::evidence(&connection).unwrap(), before);
    assert!(!committed(&connection).unwrap());
    assert!(!super::super::ledger_is_keyed(&connection).unwrap());
    assert_eq!(
        connection
            .query_row(
                "SELECT body_generation FROM sessions WHERE session_id='old-session'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM inputs WHERE input_id='orphan-input' AND state='committed'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(fs::read(source).unwrap(), source_bytes);
}

#[test]
fn changed_source_is_rejected_inside_switch_transaction_before_ledger_bridge() {
    let home = fixture();
    let connection = database(home.path());
    backup::create(
        &connection,
        home.path(),
        &home.path().join("data/runtime.sqlite3"),
        "0.32.1",
    )
    .unwrap();
    let (mut connection, prepared) = prepare(connection, home.path()).unwrap();
    connection
        .execute(
            "UPDATE sessions SET title='changed' WHERE session_id='old-session'",
            [],
        )
        .unwrap();
    let entries = product_manifest();
    let versions = super::super::validate_manifest("0.32.1", &entries).unwrap();
    assert!(matches!(
        super::super::apply_version(
            &mut connection,
            &entries[entries.iter().position(|entry| entry.key == KEY).unwrap()],
            &versions,
            &entries,
            entries.iter().position(|entry| entry.key == KEY).unwrap(),
            None,
            Some(&prepared)
        ),
        Err(MigrationError::DatabaseChanged)
    ));
    assert!(!committed(&connection).unwrap());
    assert!(!super::super::ledger_is_keyed(&connection).unwrap());
    assert_eq!(
        connection
            .query_row("SELECT body_generation FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
