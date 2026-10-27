//! 可靠终态完成后释放真实 SQL 控制；故障仍保留原回执，不级联丢失 child。
use super::*;

fn home() -> TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("f02-run-release-")
        .tempdir_in(base)
        .unwrap()
}

fn started(engine: &mut StorageEngine) -> NewStoredInput {
    let session = session_id("release-owner");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    let mut input = super::input_selections::selected_input(&session, "terminal", false);
    input.idempotency_key = Some(IdempotencyKey::new("terminal-key").unwrap());
    engine.accept_input(input.clone()).unwrap();
    claim_fixture(engine, &input);
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "terminal-start".into(),
            input_id: input.input_id.clone(),
            run_id: fixture_run_id(&input).clone(),
            session_id: session,
            message: Some(input.message.clone()),
            reasoning_effort: None,
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            created_at_ms: 2100,
        })
        .unwrap();
    input
}

fn finish(input: &NewStoredInput) -> StoredRunSettlement {
    StoredRunSettlement {
        operation_id: "terminal-finish".into(),
        run_id: fixture_run_id(input).clone(),
        session_id: input.session_id.clone(),
        status: RunStatus::Failed,
        cancel_requested: false,
        error: Some(assistant_protocol::RuntimeErrorInfo::new(
            assistant_protocol::RuntimeErrorCode::Internal,
            "fixture failed",
        )),
        queued_user_message: None,
        messages: vec![assistant_message("terminal-answer", "partial answer")],
        message_step: Some(1),
        goal_effect: None,
        proxy_report: None,
        finished_at_ms: 2200,
    }
}

#[test]
fn terminal_delete_failure_rolls_back_control_and_receipt_then_restart_finishes_once_and_allows_retry()
 {
    for trigger in [
        "CREATE TRIGGER fail_terminal_release BEFORE DELETE ON runs BEGIN SELECT RAISE(ABORT,'injected Run release failure'); END;",
        "CREATE TRIGGER fail_terminal_release BEFORE UPDATE OF user_message_id ON inputs WHEN NEW.user_message_id IS NULL AND OLD.user_message_id IS NOT NULL BEGIN SELECT RAISE(ABORT,'injected Input release failure'); END;",
    ] {
        terminal_release_failure(trigger);
    }
}

fn minimal_input_count(engine: &StorageEngine) -> i64 {
    engine.connection.query_row("SELECT COUNT(*) FROM inputs WHERE input_id='input-terminal' AND state='committed' AND priority_order IS NULL AND user_message_id IS NULL AND accepted_at_ms IS NULL AND agent_variant IS NULL AND origin IS NULL AND goal_id IS NULL AND goal_generation IS NULL AND goal_turn IS NULL AND goal_reply_route_json IS NULL AND cross_session_json IS NULL AND channel_source_json IS NULL AND agent_shell_target IS NULL AND skill_activation_json IS NULL AND queued_message_json IS NULL", [], |row|row.get(0)).unwrap()
}

fn terminal_release_failure(trigger: &str) {
    let home = home();
    let mut engine = open_engine(&home);
    let input = started(&mut engine);
    engine.connection.execute_batch(trigger).unwrap();
    assert!(engine.settle_run(finish(&input)).is_err());
    let control = engine
        .load_run_control(&input.session_id, &fixture_run_id(&input))
        .unwrap()
        .unwrap();
    assert_eq!(control.status, RunStatus::Running);
    assert!(control.finished_at_ms.is_none() && control.error.is_none());
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(engine.load_conversation(&input.session_id).is_err());
    engine
        .connection
        .execute_batch("DROP TRIGGER fail_terminal_release")
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    let recovered = engine.load_runtime().unwrap();
    assert!(recovered.runs.is_empty());
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(recovered.run_history.len(), 1);
    assert_eq!(minimal_input_count(&engine), 1);
    let restored_input = engine
        .load_input_for_id(&input.session_id, &input.input_id)
        .unwrap()
        .unwrap();
    assert_eq!(restored_input.user_message_id, input.message.id);
    assert_eq!(restored_input.accepted_at_ms, input.accepted_at_ms);
    assert_eq!(recovered.run_history[0].1.status, Some(RunStatus::Failed));
    assert_eq!(recovered.run_history[0].1.started_at_ms, Some(2100));
    assert_eq!(recovered.run_history[0].1.finished_at_ms, Some(2200));
    assert!(matches!(
        engine.accept_input(input.clone()).unwrap(),
        assistant_runtime::AcceptedInput::Historical { .. }
    ));
    let attempt = assistant_runtime::NewStoredRunAttempt {
        run_id: run_id("terminal-retry"),
        session_id: input.session_id.clone(),
        source_run_id: fixture_run_id(&input).clone(),
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        created_at_ms: 2300,
    };
    engine.connection.execute_batch("CREATE TRIGGER fail_retry_admission BEFORE INSERT ON runs BEGIN SELECT RAISE(ABORT,'injected retry admission failure'); END;").unwrap();
    assert!(engine.create_run_attempt(attempt.clone()).is_err());
    assert_eq!(
        minimal_input_count(&engine),
        1,
        "failed Run admission must roll back restored Input fields"
    );
    assert_eq!(engine.staged_append_count().unwrap(), 0);
    engine
        .connection
        .execute_batch("DROP TRIGGER fail_retry_admission")
        .unwrap();
    let retry = engine.create_run_attempt(attempt).unwrap();
    assert_eq!(retry.attempt, 2);
    assert_eq!(
        minimal_input_count(&engine),
        0,
        "actual retry admission restores temporary execution fields"
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT state FROM inputs WHERE input_id='input-terminal'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "committed"
    );
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "retry-start".into(),
            input_id: input.input_id.clone(),
            run_id: retry.run_id.clone(),
            session_id: input.session_id.clone(),
            message: None,
            reasoning_effort: None,
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            created_at_ms: 2400,
        })
        .unwrap();
    finish_fixture_run(
        &mut engine,
        input.session_id.as_str(),
        retry.run_id.as_str(),
        RunStatus::Completed,
        None,
        2500,
    );
    drop(engine);
    let mut engine = open_engine(&home);
    let recovered = engine.load_runtime().unwrap();
    assert!(recovered.runs.is_empty());
    assert_eq!(recovered.run_history.len(), 2);
    assert_eq!(
        minimal_input_count(&engine),
        1,
        "retry completion releases temporary fields again"
    );
    assert_eq!(
        engine
            .load_conversation(&input.session_id)
            .unwrap()
            .messages
            .len(),
        2
    );
    let path = body_path(&engine.session_directory(&input.session_id).unwrap(), 1);
    let bytes = fs::read(&path).unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    engine.load_runtime().unwrap();
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn parent_settlement_does_not_cascade_a_live_child_before_its_own_file_completion() {
    let home = home();
    let mut engine = open_engine(&home);
    let input = started(&mut engine);
    let child = child_task_id("live-child");
    engine
        .create_child_task(NewStoredChildTask {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            context_window_tokens: 8192,
            child_task_id: child.clone(),
            session_id: input.session_id.clone(),
            parent_run_id: fixture_run_id(&input).clone(),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("standalone-child-call")
                .unwrap(),
            title: "child".into(),
            system_prompt: SystemPromptSnapshot::default(),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2150,
        })
        .unwrap();
    let path = body_path(&engine.session_directory(&input.session_id).unwrap(), 1);
    let bytes = fs::read(&path).unwrap();
    assert!(engine.settle_run(finish(&input)).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    for table in ["runs", "child_tasks"] {
        assert_eq!(
            engine
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(engine.staged_append_count().unwrap(), 0);
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "child-cancel".into(),
            child_task_id: child,
            session_id: input.session_id.clone(),
            status: ChildTaskStatus::Cancelled,
            cancel_requested: true,
            error: None,
            messages: Vec::new(),
            final_message_id: None,
            finished_at_ms: 2175,
        })
        .unwrap();
    engine.settle_run(finish(&input)).unwrap();
    assert!(engine.load_runtime().unwrap().runs.is_empty());
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .load_child_history_catalog(&input.session_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn finished_input_uses_file_metadata_and_rejects_reintroduced_sql_history_on_next_admission() {
    let home = home();
    let mut engine = open_engine(&home);
    let input = started(&mut engine);
    engine.settle_run(finish(&input)).unwrap();
    assert_eq!(minimal_input_count(&engine), 1);
    engine.connection.execute("UPDATE inputs SET user_message_id='poison-user',accepted_at_ms=999999,agent_variant='plan',origin='user',channel_source_json='{' WHERE input_id='input-terminal'",[]).unwrap();
    let changes = engine.connection.total_changes();
    let restored = engine
        .load_input_for_id(&input.session_id, &input.input_id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.user_message_id, input.message.id);
    assert_eq!(restored.accepted_at_ms, input.accepted_at_ms);
    assert_eq!(restored.agent_variant, input.agent_variant);
    assert_eq!(restored.channel_source, input.channel_source);
    assert!(matches!(
        engine.accept_input(input).unwrap(),
        assistant_runtime::AcceptedInput::Historical { .. }
    ));
    assert_eq!(
        engine.connection.total_changes(),
        changes,
        "history reading never restores SQL controls"
    );
    drop(engine);
    assert!(StorageEngine::open(home.path()).is_err());
}
