//! 暂存、领取和旧队列迁移使用新建的一次性 Runtime Home；不启动模型或访问用户目录。
use super::*;

fn home() -> TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("f02-queue-claim-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn queue_is_retractable_without_a_run_and_claim_creates_exactly_one_execution() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("queue-owner");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    let mut first = super::input_selections::selected_input(&session, "queue-first", false);
    first.idempotency_key = Some(IdempotencyKey::new("queue-first-key").unwrap());
    let second = super::input_selections::selected_input(&session, "queue-second", false);
    let path = body_path(&engine.session_directory(&session).unwrap(), 1);
    let original_body = fs::read(&path).unwrap();
    for input in [&first, &second] {
        let accepted = engine.accept_input(input.clone()).unwrap();
        assert!(matches!(
            accepted,
            assistant_runtime::AcceptedInput::Control { run: None, .. }
        ));
    }
    let duplicate = engine.accept_input(first.clone()).unwrap();
    assert!(duplicate.is_duplicate());
    assert!(duplicate.run_id().is_none());
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        engine
            .load_conversation(&session)
            .unwrap()
            .messages
            .is_empty()
    );
    assert_eq!(fs::read(&path).unwrap(), original_body);
    engine
        .prioritize_queued_input(QueuePriorityChange {
            session_id: session.clone(),
            input_id: second.input_id.clone(),
        })
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    assert_eq!(engine.load_inputs().unwrap()[0].input_id, second.input_id);
    assert!(
        engine
            .load_session_state(&session)
            .unwrap()
            .state
            .runs
            .is_empty()
    );
    engine
        .cancel_queued_input(&session, &first.input_id)
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), original_body);
    claim_fixture(&mut engine, &second);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        engine
            .cancel_queued_input(&session, &second.input_id)
            .is_err()
    );
    assert!(
        engine
            .prioritize_queued_input(QueuePriorityChange {
                session_id: session.clone(),
                input_id: second.input_id.clone()
            })
            .is_err()
    );
    let run = engine
        .load_run_control(&session, &fixture_run_id(&second))
        .unwrap()
        .unwrap();
    assert!(engine.claim_input(run).is_err());
    super::input_selections::complete_selected_turn(&mut engine, &second);
    assert!(
        engine
            .load_run_control(&session, &fixture_run_id(&second))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        engine.load_conversation(&session).unwrap().messages.len(),
        2
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM inputs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn upgrade_detaches_only_unstarted_queue_controls_and_removes_successful_backup() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("legacy-queue-owner");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    let input = super::input_selections::selected_input(&session, "legacy-queue", true);
    engine.accept_input(input.clone()).unwrap();
    let before = engine.load_inputs().unwrap();
    // 只在已核实的一次性隔离库构造旧版入队形态；此控制从未开始，没有正文或工具回执。
    engine.connection.execute("INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms) VALUES('old-queued-run',?1,?2,1,'accepted',0,2000)",rusqlite::params![session.as_str(),input.input_id.as_str()]).unwrap();
    engine
        .connection
        .execute("UPDATE skill_activations SET run_id='old-queued-run'", [])
        .unwrap();
    engine.connection.execute("UPDATE inputs SET skill_activation_json=json_set(skill_activation_json,'$.run_id','old-queued-run') WHERE input_id=?1",[input.input_id.as_str()]).unwrap();
    engine.connection.execute_batch("DELETE FROM schema_migrations WHERE migration_key='conversation_usage_snapshots_v1'; DROP TABLE conversation_usage_snapshots;").unwrap();
    assert_eq!(
        engine
            .connection
            .execute(
                "DELETE FROM schema_migrations WHERE migration_key='f02_queue_claim_v1'",
                []
            )
            .unwrap(),
        1
    );
    drop(engine);
    let mut engine = open_engine(&home);
    assert_eq!(engine.load_inputs().unwrap(), before);
    assert!(
        engine
            .load_session_state(&session)
            .unwrap()
            .state
            .runs
            .is_empty()
    );
    assert!(
        engine
            .load_skill_activations()
            .unwrap()
            .iter()
            .all(|activation| activation.run_id.is_none())
    );
    assert_eq!(
        fs::read_dir(home.path().join("backups/database"))
            .unwrap()
            .count(),
        0
    );
    engine
        .cancel_queued_input(&session, &input.input_id)
        .unwrap();
    assert!(
        engine
            .load_conversation(&session)
            .unwrap()
            .messages
            .is_empty()
    );
}

#[test]
fn queue_upgrade_preserves_an_already_claimed_start_receipt_for_normal_recovery() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("pending-start-owner");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    let input = super::input_selections::selected_input(&session, "pending-start", false);
    engine.accept_input(input.clone()).unwrap();
    claim_fixture(&mut engine, &input);
    engine.connection.execute_batch("CREATE TRIGGER pause_claimed_start BEFORE DELETE ON body_appends BEGIN SELECT RAISE(ABORT,'injected incomplete start'); END;").unwrap();
    assert!(
        engine
            .commit_user_message(UserMessageCommit {
                context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                shell: None,
                operation_id: "pending-claimed-start".into(),
                input_id: input.input_id.clone(),
                run_id: fixture_run_id(&input),
                session_id: session.clone(),
                message: Some(input.message.clone()),
                reasoning_effort: None,
                created_at_ms: 2100,
            })
            .is_err()
    );
    assert_eq!(engine.staged_append_count().unwrap(), 1);
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_claimed_start")
        .unwrap();
    engine.connection.execute_batch("DELETE FROM schema_migrations WHERE migration_key='conversation_usage_snapshots_v1'; DROP TABLE conversation_usage_snapshots;").unwrap();
    assert_eq!(
        engine
            .connection
            .execute(
                "DELETE FROM schema_migrations WHERE migration_key='f02_queue_claim_v1'",
                []
            )
            .unwrap(),
        1
    );
    drop(engine);
    let mut engine = open_engine(&home);
    let loaded = engine.prepare_session_execution(&session).unwrap();
    assert_eq!(loaded.state.runs.len(), 1);
    assert_eq!(loaded.state.runs[0].run_id, fixture_run_id(&input));
    assert_eq!(
        loaded.state.inputs[0].state,
        assistant_runtime::StoredInputState::Committed
    );
    assert_eq!(engine.staged_append_count().unwrap(), 0);
    assert_eq!(
        engine.load_conversation(&session).unwrap().messages.len(),
        1
    );
}
