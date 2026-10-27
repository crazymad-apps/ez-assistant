//! 真实队列的选择临时留 SQL，可靠消费后只从文件恢复；每个场景新建隔离 Home。
use super::*;

pub(super) fn selected_input(session: &SessionId, suffix: &str, skill: bool) -> NewStoredInput {
    let input_id = InputId::new(format!("input-{suffix}")).unwrap();
    let mut message = raw_user_message(&format!("user-{suffix}"), suffix);
    let activation = skill.then(|| {
        message.parts.push(UserPart::InternalContext(
            InternalContextPart::new(
                PartId::new(format!("skill-part-{suffix}")).unwrap(),
                format!("skill-boundary-{suffix}"),
                "skill_activation",
                "SKILL_ACTIVATION_V1\nReview carefully",
            )
            .unwrap(),
        ));
        StoredSkillActivation {
            activation_id: format!("activation-{suffix}"),
            session_id: session.clone(),
            owner: SkillActivationOwner::Session(session.clone()),
            run_id: None,
            input_id: Some(input_id.clone()),
            message_id: message.id.clone(),
            name: SkillName::parse("review").unwrap(),
            catalog_revision: format!("sha256-v1:{}", "b".repeat(64)),
            definition_digest: format!("sha256-v1:{}", "a".repeat(64)),
            trigger: SkillActivationTrigger::User,
            created_at_ms: 2000,
        }
    });
    NewStoredInput {
        feedback_answer: None,
        agent_shell_target: None,
        input_id: input_id.clone(),
        session_id: session.clone(),
        idempotency_key: None,
        agent_variant: assistant_protocol::AgentVariant::Build,
        origin: InputOrigin::User,
        goal_binding: None,
        cross_session: None,
        channel_source: Some(desktop_channel_source()),
        skill_activation: activation,
        mcp_selection: Some(StoredMcpSelection {
            selection_id: format!("selection-{suffix}"),
            session_id: session.clone(),
            input_id: Some(input_id),
            message_id: message.id.clone(),
            server_key: McpServerKey::new("github").unwrap(),
            display_name: "GitHub".into(),
            created_at_ms: 2000,
        }),
        message,
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: 2000,
    }
}

pub(super) fn complete_selected_turn(engine: &mut StorageEngine, input: &NewStoredInput) {
    if engine
        .load_run_control(&input.session_id, &fixture_run_id(input))
        .unwrap()
        .is_none()
    {
        claim_fixture(engine, input);
    }
    engine.commit_user_message(start(input)).unwrap();
    let mut finish = settlement(input, false);
    finish.messages.push(assistant_message(
        &format!("assistant-{}", input.input_id),
        "done",
    ));
    engine.settle_run(finish).unwrap();
}

fn start(input: &NewStoredInput) -> UserMessageCommit {
    UserMessageCommit {
        operation_id: format!("start-{}", input.input_id),
        input_id: input.input_id.clone(),
        run_id: fixture_run_id(input).clone(),
        session_id: input.session_id.clone(),
        message: Some(input.message.clone()),
        context_management_mode: agent_model::ContextManagementMode::LocalSummary,
        shell: None,
        reasoning_effort: None,
        created_at_ms: 2100,
    }
}

fn settlement(input: &NewStoredInput, prestart: bool) -> StoredRunSettlement {
    StoredRunSettlement {
        operation_id: format!("finish-{}", input.input_id),
        run_id: fixture_run_id(input).clone(),
        session_id: input.session_id.clone(),
        status: if prestart {
            RunStatus::Cancelled
        } else {
            RunStatus::Completed
        },
        cancel_requested: prestart,
        error: None,
        queued_user_message: prestart.then(|| input.message.clone()),
        messages: Vec::new(),
        message_step: None,
        goal_effect: None,
        proxy_report: None,
        finished_at_ms: 2200,
    }
}

#[test]
fn consumed_selections_recover_from_files_while_another_input_remains_queued() {
    for prestart in [false, true] {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        fs::create_dir_all(&base).unwrap();
        let root = tempfile::Builder::new()
            .prefix("f02-queue-selections-")
            .tempdir_in(base)
            .unwrap();
        let mut engine = open_engine(&root);
        let session = session_id("selection-owner");
        let mut consumed = selected_input(&session, "consumed", true);
        let waiting = selected_input(&session, "waiting", true);
        let mut created = new_session(session.as_str(), &engine.sessions_directory);
        created.materialization_key =
            Some(IdempotencyKey::new("selection-materialization").unwrap());
        let materialization = NewStoredSessionMaterialization {
            session: created,
            attachments: Vec::new(),
            input: consumed.clone(),
        };
        engine.materialize_session(materialization.clone()).unwrap();
        engine.accept_input(waiting.clone()).unwrap();
        let waiting_row = |engine: &StorageEngine| {
            engine.connection.query_row(
            "SELECT queued_message_json,skill_activation_json,priority_order FROM inputs WHERE input_id='input-waiting'",
            [], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, i64>(2)?)),
        ).unwrap()
        };
        let original_waiting = waiting_row(&engine);
        drop(engine);
        let mut engine = open_engine(&root);
        assert_eq!(waiting_row(&engine), original_waiting);
        assert_eq!(engine.load_skill_activations().unwrap().len(), 2);
        assert_eq!(engine.load_mcp_input_selections().unwrap().len(), 2);
        claim_fixture(&mut engine, &consumed);
        consumed.skill_activation.as_mut().unwrap().run_id = Some(fixture_run_id(&consumed));
        engine.connection.execute_batch("CREATE TRIGGER fail_selection_receipt BEFORE DELETE ON body_appends BEGIN SELECT RAISE(ABORT,'injected completion failure'); END;").unwrap();
        if prestart {
            assert!(engine.settle_run(settlement(&consumed, true)).is_err());
        } else {
            assert!(engine.commit_user_message(start(&consumed)).is_err());
        }
        for table in ["skill_activations", "mcp_input_selections"] {
            assert_eq!(
                engine
                    .connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                2
            );
        }
        assert_eq!(engine.connection.query_row("SELECT COUNT(*) FROM inputs WHERE state='queued' AND queued_message_json IS NOT NULL AND skill_activation_json IS NOT NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(
            engine
                .connection
                .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(engine.load_conversation(&session).is_err());
        engine
            .connection
            .execute_batch("DROP TRIGGER fail_selection_receipt")
            .unwrap();
        drop(engine);
        let mut engine = open_engine(&root);
        let recovered = engine.load_runtime().unwrap();
        assert_eq!(recovered.skill_activations.len(), 2);
        assert_eq!(recovered.mcp_input_selections.len(), 2);
        assert_eq!(
            recovered
                .inputs
                .iter()
                .find(|input| input.input_id == consumed.input_id)
                .unwrap()
                .skill_activation,
            consumed.skill_activation
        );
        for table in ["skill_activations", "mcp_input_selections"] {
            assert_eq!(
                engine
                    .connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE input_id='input-waiting'"),
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            assert_eq!(
                engine
                    .connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE input_id='input-consumed'"),
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        assert_eq!(engine.connection.query_row("SELECT COUNT(*) FROM inputs WHERE state='committed' AND skill_activation_json IS NOT NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(
            engine
                .connection
                .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(waiting_row(&engine), original_waiting);
        let loaded = engine.load_session_state(&session).unwrap();
        assert_eq!(loaded.state.skill_activations, recovered.skill_activations);
        if !prestart {
            engine.settle_run(settlement(&consumed, false)).unwrap();
        }
        drop(engine);
        let mut engine = open_engine(&root);
        let changes = engine.connection.total_changes();
        assert_eq!(
            engine
                .materialize_session(materialization)
                .unwrap()
                .accepted
                .input_id(),
            &consumed.input_id
        );
        assert_eq!(engine.connection.total_changes(), changes);
        let history = engine
            .load_conversation_history(ConversationOwner::MainSession {
                session_id: session.clone(),
            })
            .unwrap();
        assert_eq!(
            history.facts.skill_activations,
            vec![consumed.skill_activation.unwrap()]
        );
        assert_eq!(
            history.facts.mcp_selections,
            vec![consumed.mcp_selection.unwrap()]
        );
        assert_eq!(
            engine.load_conversation(&session).unwrap().messages,
            vec![ConversationMessage::User(consumed.message)]
        );
        assert_eq!(waiting_row(&engine), original_waiting);
        // 只在此一次性夹具注入过期 SQL：正常读取不能消费其无效载荷，下一次准入必须拒绝。
        engine.connection.execute_batch(
            "INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms,started_at_ms)
             VALUES('run-consumed','selection-owner','input-consumed',1,'completed',0,2000,2200);
             INSERT INTO skill_activations(activation_id,session_id,owner_kind,owner_id,run_id,input_id,message_id,name,catalog_revision,definition_digest,trigger,created_at_ms)
             VALUES('stale-activation','selection-owner','session','selection-owner','run-consumed','input-consumed','user-consumed','review','','','user',0);
             INSERT INTO mcp_input_selections(selection_id,session_id,input_id,message_id,server_key,display_name,created_at_ms)
             VALUES('stale-selection','selection-owner','input-consumed','user-consumed','stale','',0);"
        ).unwrap();
        assert!(
            engine
                .connection
                .execute(
                    "UPDATE inputs SET skill_activation_json='{' WHERE input_id='input-consumed'",
                    []
                )
                .is_err(),
            "minimal completed identities must reject historical payloads"
        );
        assert_eq!(engine.load_skill_activations().unwrap().len(), 1);
        assert_eq!(engine.load_mcp_input_selections().unwrap().len(), 1);
        assert_eq!(
            engine
                .load_session_state(&session)
                .unwrap()
                .state
                .skill_activations,
            recovered.skill_activations
        );
        assert_eq!(waiting_row(&engine), original_waiting);
        drop(engine);
        assert!(StorageEngine::open(root.path()).is_err());
    }
}
