//! 身份归当前 owner 的可靠文件；SQL 控制释放后仍禁止覆盖历史，幂等命中保持只读。
use super::*;

fn home() -> TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("f02-input-identity-")
        .tempdir_in(base)
        .unwrap()
}

fn start(engine: &mut StorageEngine, input: &NewStoredInput) {
    engine.accept_input(input.clone()).unwrap();
    claim_fixture(engine, input);
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: format!("start-{}", input.input_id),
            input_id: input.input_id.clone(),
            run_id: fixture_run_id(input).clone(),
            session_id: input.session_id.clone(),
            message: Some(input.message.clone()),
            reasoning_effort: None,
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            created_at_ms: 2100,
        })
        .unwrap();
}

#[test]
fn original_materialization_identity_is_reserved_after_history_clear_and_restart() {
    let root = home();
    let mut engine = open_engine(&root);
    let original = completed(&mut engine, "reserved-original", SessionRole::Standard);
    assert_eq!(engine.connection.execute(
        "UPDATE sessions SET materialization_key=?1, materialization_input_id=?2 WHERE session_id=?3",
        rusqlite::params!["original-materialization", original.input_id.as_str(), original.session_id.as_str()],
    ).unwrap(), 1);
    let stored = engine
        .load_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.session_id == original.session_id)
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM inputs WHERE session_id=?1",
                [original.session_id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    engine
        .clear_session_history(assistant_runtime::SessionHistoryClear {
            operation_id: IdempotencyKey::new("clear-reserved-original").unwrap(),
            session_id: original.session_id.clone(),
            expected_generation: stored.body_generation,
            system_prompt: stored.system_prompt,
            environment: stored.environment,
            expected_role: SessionRole::Standard,
            changed_at_ms: 3000,
        })
        .unwrap();
    for pass in 0..2 {
        let changes = engine.connection.total_changes();
        let mut candidate =
            super::input_selections::selected_input(&original.session_id, "replacement", false);
        candidate.mcp_selection = None;
        candidate.input_id = original.input_id.clone();
        assert_eq!(
            engine.accept_input(candidate).unwrap_err().kind(),
            StoreErrorKind::Conflict
        );
        let mut command = super::commands::request(&original.session_id, "replacement-command");
        command.input_id = original.input_id.clone();
        assert_eq!(
            engine.accept_session_command(command).unwrap_err().kind(),
            StoreErrorKind::Conflict
        );
        assert_eq!(
            engine.connection.total_changes(),
            changes,
            "rejected identities perform no writes"
        );
        for table in ["inputs", "runs", "body_appends"] {
            assert_eq!(
                engine
                    .connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE session_id=?1"),
                        [original.session_id.as_str()],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            engine
                .connection
                .query_row(
                    "SELECT materialization_input_id FROM sessions WHERE session_id=?1",
                    [original.session_id.as_str()],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            original.input_id.as_str()
        );
        assert!(
            engine
                .load_conversation(&original.session_id)
                .unwrap()
                .messages
                .is_empty()
        );
        assert_eq!(engine.staged_append_count().unwrap(), 0);
        if pass == 0 {
            drop(engine);
            engine = open_engine(&root);
        }
    }
}

fn completed(engine: &mut StorageEngine, name: &str, role: SessionRole) -> NewStoredInput {
    let mut session = new_session(name, &engine.sessions_directory);
    session.role = role;
    engine.create_session(session).unwrap();
    let mut input = super::input_selections::selected_input(&session_id(name), name, false);
    input.mcp_selection = None;
    input.idempotency_key = Some(IdempotencyKey::new(format!("key-{name}")).unwrap());
    start(engine, &input);
    engine
        .append_messages(AppendRequest {
            operation_id: format!("answer-{name}"),
            session_id: input.session_id.clone(),
            run_id: fixture_run_id(&input).clone(),
            messages: vec![assistant_message(&format!("answer-{name}"), "done")],
            message_step: Some(1),
            created_at_ms: 2200,
        })
        .unwrap();
    finish_fixture_run(
        engine,
        name,
        fixture_run_id(&input).as_str(),
        RunStatus::Failed,
        None,
        2300,
    );
    input
}

fn assert_collisions(
    engine: &mut StorageEngine,
    session: &SessionId,
    original: &NewStoredInput,
    command: &assistant_runtime::NewStoredSessionCommand,
) {
    let generation = engine.session_generation(session).unwrap();
    let path = body_path(&engine.session_directory(session).unwrap(), generation);
    let bytes = fs::read(&path).unwrap();
    let changes = engine.connection.total_changes();
    for kind in [0, 2, 3, 4, 5] {
        let mut candidate =
            super::input_selections::selected_input(session, &format!("collision-{kind}"), false);
        candidate.mcp_selection = None;
        match kind {
            0 => candidate.input_id = original.input_id.clone(),
            2 => candidate.message.id = original.message.id.clone(),
            3 => candidate.input_id = command.input_id.clone(),
            4 => candidate.message.id = command.user_message_id.clone(),
            _ => {
                candidate.message.id =
                    MessageId::new(format!("answer-{}", original.session_id)).unwrap()
            }
        }
        assert_eq!(
            engine.accept_input(candidate).unwrap_err().kind(),
            StoreErrorKind::Conflict
        );
    }
    for kind in 0..3 {
        let mut candidate = super::commands::request(session, &format!("command-collision-{kind}"));
        match kind {
            0 => candidate.user_message_id = original.message.id.clone(),
            1 => candidate.input_id = original.input_id.clone(),
            _ => candidate.input_id = command.input_id.clone(),
        }
        assert_eq!(
            engine.accept_session_command(candidate).unwrap_err().kind(),
            StoreErrorKind::Conflict
        );
    }
    assert_eq!(engine.connection.total_changes(), changes);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(engine.staged_append_count().unwrap(), 0);
}

#[test]
fn input_and_command_admission_keep_file_identity_protection_after_control_release_and_restart() {
    let home = home();
    let mut engine = open_engine(&home);
    let original = completed(&mut engine, "identity-owner", SessionRole::Standard);
    let command = super::commands::request(&original.session_id, "original-command");
    engine.accept_session_command(command.clone()).unwrap();
    engine
        .commit_session_command(super::commands::commit(&command))
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_collisions(&mut engine, &original.session_id, &original, &command);
    assert_eq!(
        engine
            .create_run_attempt(assistant_runtime::NewStoredRunAttempt {
                session_id: original.session_id.clone(),
                source_run_id: fixture_run_id(&original).clone(),
                run_id: fixture_run_id(&original).clone(),
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                created_at_ms: 4000,
            })
            .unwrap_err()
            .kind(),
        StoreErrorKind::Conflict
    );
    let changes = engine.connection.total_changes();
    assert!(matches!(
        engine.accept_input(original.clone()).unwrap(),
        assistant_runtime::AcceptedInput::Historical { .. }
    ));
    assert!(
        engine
            .accept_session_command(command.clone())
            .unwrap()
            .is_duplicate
    );
    assert_eq!(engine.connection.total_changes(), changes);
    drop(engine);
    let mut reopened = open_engine(&home);
    assert_collisions(&mut reopened, &original.session_id, &original, &command);
    // 可靠身份属于本 owner；其他会话中相同消息 ID 不得阻止本会话新输入。
    let other = session_id("identity-other");
    reopened
        .create_session(new_session(other.as_str(), &reopened.sessions_directory))
        .unwrap();
    let mut candidate = super::input_selections::selected_input(&other, "other-input", false);
    candidate.message.id = original.message.id;
    candidate.mcp_selection = None;
    reopened.accept_input(candidate).unwrap();
}

#[test]
fn inherited_file_identities_still_protect_the_owner_after_source_deletion_without_sql_history() {
    let home = home();
    let mut engine = open_engine(&home);
    let original = completed(&mut engine, "inherited-source", SessionRole::Standard);
    let command = super::commands::request(&original.session_id, "inherited-command");
    engine.accept_session_command(command.clone()).unwrap();
    engine
        .commit_session_command(super::commands::commit(&command))
        .unwrap();
    let transcript = engine.load_conversation(&original.session_id).unwrap();
    let mut facts = engine
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: original.session_id.clone(),
        })
        .unwrap()
        .facts;
    for input in &mut facts.inputs {
        input.inherited = true;
    }
    for run in &mut facts.runs {
        run.inherited = true;
    }
    for origin in &mut facts.message_origins {
        origin.inherited = true;
    }
    let target = session_id("inherited-owner");
    for command in &mut facts.commands {
        command.session_id = target.clone();
    }
    engine
        .create_session(new_session(target.as_str(), &engine.sessions_directory))
        .unwrap();
    // M0 夹具直接准备只读继承文件；完整 Fork 的裁切和资源复制仍由 M1 验证。
    let plan = engine
        .begin_replacement(target.clone(), transcript.clone(), 4000)
        .unwrap();
    let path = body_path(
        &engine.session_directory(&target).unwrap(),
        plan.new_generation,
    );
    fs::remove_file(&path).unwrap();
    conversation::write_generation(
        &path,
        &ConversationOwner::MainSession {
            session_id: target.clone(),
        },
        plan.new_generation,
        &transcript,
        &facts,
    )
    .unwrap();
    engine.commit_replacement(&plan).unwrap();
    let impact = engine
        .inspect_session_deletion(&original.session_id)
        .unwrap();
    engine
        .delete_session(SessionDeletion {
            session_id: original.session_id.clone(),
            operation_id: "delete-inherited-source".into(),
            expected_impact: impact,
        })
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM inputs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_collisions(&mut engine, &target, &original, &command);
    drop(engine);
    let mut reopened = open_engine(&home);
    assert_collisions(&mut reopened, &target, &original, &command);
}

#[test]
fn rewrite_cannot_reuse_an_identity_even_when_that_history_would_be_truncated() {
    let home = home();
    let mut engine = open_engine(&home);
    let original = completed(&mut engine, "rewrite-identity", SessionRole::Standard);
    for kind in 0..3 {
        let mut input = super::input_selections::selected_input(
            &original.session_id,
            &format!("rewrite-new-{kind}"),
            false,
        );
        input.mcp_selection = None;
        match kind {
            0 => input.input_id = original.input_id.clone(),
            1 => {}
            _ => input.message.id = original.message.id.clone(),
        }
        let old = engine.load_conversation(&original.session_id).unwrap();
        let changes = engine.connection.total_changes();
        let error = engine
            .rewrite_from_user(assistant_runtime::ConversationRewrite {
                run_id: if kind == 1 {
                    fixture_run_id(&original)
                } else {
                    fixture_run_id(&input)
                },
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                session_id: original.session_id.clone(),
                target_user_message_id: original.message.id.clone(),
                conversation: ConversationSnapshot::new(vec![ConversationMessage::User(
                    input.message.clone(),
                )]),
                input,
                goal_effect: None,
                changed_at_ms: 4000,
            })
            .unwrap_err();
        assert_eq!(error.kind(), StoreErrorKind::Conflict);
        assert_eq!(engine.connection.total_changes(), changes);
        assert_eq!(engine.load_conversation(&original.session_id).unwrap(), old);
        assert_eq!(engine.session_generation(&original.session_id).unwrap(), 1);
    }
}

#[test]
fn proxy_report_rechecks_target_file_identity_before_finalization_and_recovery() {
    let home = home();
    let mut engine = open_engine(&home);
    let previous = completed(&mut engine, "report-controller", SessionRole::Controller);
    let source = session_id("report-worker");
    engine
        .create_session(new_session(source.as_str(), &engine.sessions_directory))
        .unwrap();
    let mut worker = super::input_selections::selected_input(&source, "report-worker", false);
    worker.mcp_selection = None;
    start(&mut engine, &worker);
    engine
        .set_session_proxy(SessionProxyChange {
            target_session_id: source.clone(),
            controller_session_id: previous.session_id.clone(),
            enabled: true,
            changed_at_ms: 2400,
        })
        .unwrap();
    let mut report =
        super::input_selections::selected_input(&previous.session_id, "report-result", false);
    report.mcp_selection = None;
    report.origin = InputOrigin::Runtime;
    report.channel_source = None;
    report.idempotency_key = Some(IdempotencyKey::new("report-key").unwrap());
    report.cross_session = Some(CrossSessionInputEnvelope {
        binding: CrossSessionInputBinding::ProxyReport {
            source_session_id: source.clone(),
            source_run_id: fixture_run_id(&worker).clone(),
            source_goal_id: None,
            source_run_status: RunStatus::Completed,
        },
        reply_route: assistant_runtime::ReplyRoute::SessionDefault,
    });
    report.message.origin = UserMessageOrigin::Runtime;
    report.message.parts.push(UserPart::InternalContext(
        InternalContextPart::new(
            PartId::new("report-context").unwrap(),
            "report-boundary",
            "proxy_report",
            "reliable report",
        )
        .unwrap(),
    ));
    let mut collision = report.clone();
    collision.input_id = previous.input_id.clone();
    let source_path = body_path(&engine.session_directory(&source).unwrap(), 1);
    let source_bytes = fs::read(&source_path).unwrap();
    assert_eq!(
        engine
            .settle_run(StoredRunSettlement {
                operation_id: "report-preflight".into(),
                session_id: source.clone(),
                run_id: fixture_run_id(&worker).clone(),
                status: RunStatus::Completed,
                cancel_requested: false,
                error: None,
                queued_user_message: None,
                messages: Vec::new(),
                message_step: None,
                goal_effect: None,
                proxy_report: Some(Box::new(collision)),
                finished_at_ms: 2500,
            })
            .unwrap_err()
            .kind(),
        StoreErrorKind::Conflict
    );
    assert_eq!(engine.staged_append_count().unwrap(), 0);
    assert_eq!(fs::read(&source_path).unwrap(), source_bytes);

    engine
        .stage_append_for(
            AppendRequest {
                operation_id: "report-pending".into(),
                session_id: source.clone(),
                run_id: fixture_run_id(&worker).clone(),
                messages: Vec::new(),
                message_step: None,
                created_at_ms: 2500,
            },
            AppendPurpose::RunSettlement {
                status: RunStatus::Completed,
                cancel_requested: false,
                error: None,
                queued_user_message: None,
                goal_effect: None,
                proxy_report: Some(Box::new(report.clone())),
            },
        )
        .unwrap();
    let mut intervening =
        super::input_selections::selected_input(&previous.session_id, "intervening", false);
    intervening.mcp_selection = None;
    intervening.input_id = report.input_id.clone();
    start(&mut engine, &intervening);
    finish_fixture_run(
        &mut engine,
        previous.session_id.as_str(),
        fixture_run_id(&intervening).as_str(),
        RunStatus::Completed,
        None,
        2600,
    );
    assert_eq!(
        engine
            .complete_staged_append("report-pending")
            .unwrap_err()
            .kind(),
        StoreErrorKind::Conflict
    );
    let pending_bytes = fs::read(&source_path).unwrap();
    assert!(engine.load_conversation(&source).is_err());
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT status FROM runs WHERE run_id=?1",
                [fixture_run_id(&worker).as_str()],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "running"
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM inputs WHERE input_id=?1",
                [report.input_id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(engine);
    let mut reopened = open_engine(&home);
    let recovered = reopened.load_runtime().unwrap();
    assert_eq!(
        recovered
            .sessions
            .iter()
            .find(|session| session.session_id == source)
            .unwrap()
            .conversation_state,
        StoredConversationState::Unavailable
    );
    assert_eq!(reopened.staged_append_count().unwrap(), 1);
    assert_eq!(fs::read(source_path).unwrap(), pending_bytes);
    assert_eq!(
        reopened
            .connection
            .query_row(
                "SELECT COUNT(*) FROM inputs WHERE input_id=?1",
                [report.input_id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        reopened
            .load_conversation_history(ConversationOwner::MainSession {
                session_id: previous.session_id
            })
            .unwrap()
            .facts
            .runs
            .len(),
        2
    );
}
