//! 正式 Store 写入产生的可靠历史；删除仅限本测试新建的隔离 SQL 夹具。

use super::*;

fn home() -> TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("f02-file-facts-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn fork_uses_reliable_end_positions_for_empty_retries_even_when_clocks_reverse() {
    let home = home();
    let mut engine = open_engine(&home);
    let source = session_id("empty-retry-boundary-source");
    let directories = engine.sessions_directory.clone();
    engine
        .create_session(new_session(source.as_str(), &directories))
        .unwrap();
    write_fixture_turn(&mut engine, &source, "boundary-first", 1000, false);
    let first = run_id("run-boundary-first");
    finish_fixture_run(
        &mut engine,
        source.as_str(),
        first.as_str(),
        RunStatus::Failed,
        None,
        1100,
    );
    let input = InputId::new("input-boundary-first").unwrap();
    let mut previous = first;
    for (name, at) in [("empty-before-cut", 2000), ("empty-after-cut", 500)] {
        let id = run_id(name);
        engine
            .create_run_attempt(assistant_runtime::NewStoredRunAttempt {
                run_id: id.clone(),
                source_run_id: previous,
                session_id: source.clone(),
                approval_mode: assistant_protocol::ApprovalMode::Ask,
                created_at_ms: at,
            })
            .unwrap();
        engine
            .commit_user_message(UserMessageCommit {
                context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                shell: None,
                operation_id: format!("start-{name}"),
                input_id: input.clone(),
                run_id: id.clone(),
                session_id: source.clone(),
                message: None,
                reasoning_effort: None,
                created_at_ms: at + 1,
            })
            .unwrap();
        finish_fixture_run(
            &mut engine,
            source.as_str(),
            name,
            RunStatus::Failed,
            None,
            at + 2,
        );
        previous = id;
        if name == "empty-before-cut" {
            commit_completed_turn(&mut engine, &source, "boundary-other", 3000);
        }
    }
    drop(engine);
    let mut engine = open_engine(&home);
    let facts = engine
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: source.clone(),
        })
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .runs
            .iter()
            .find(|run| run.run_id.as_str() == "empty-before-cut")
            .unwrap()
            .finished_after_message_id
            .as_ref()
            .unwrap()
            .as_str(),
        "assistant-boundary-first"
    );
    assert_eq!(
        facts
            .runs
            .iter()
            .find(|run| run.run_id.as_str() == "empty-after-cut")
            .unwrap()
            .finished_after_message_id
            .as_ref()
            .unwrap()
            .as_str(),
        "assistant-boundary-other"
    );
    let conversation = engine.load_conversation(&source).unwrap();
    let target = session_id("empty-retry-boundary-target");
    engine
        .fork_session(SessionFork {
            source_session_id: source,
            source_generation: 1,
            session: new_session(target.as_str(), &directories),
            conversation,
            attachments: Vec::new(),
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            mcp_selections: Vec::new(),
            session_commands: Vec::new(),
            work_plan: None,
            goal: None,
        })
        .unwrap();
    let facts = engine
        .load_conversation_history(ConversationOwner::MainSession { session_id: target })
        .unwrap()
        .facts;
    assert_eq!(facts.runs.len(), 3);
    assert!(
        facts
            .runs
            .iter()
            .any(|run| run.run_id.as_str() == "empty-before-cut")
    );
    assert!(
        !facts
            .runs
            .iter()
            .any(|run| run.run_id.as_str() == "empty-after-cut")
    );
}

#[test]
fn fork_child_files_and_images_are_independent_and_missing_resources_are_explicit() {
    for missing in ["none", "body", "image"] {
        let home = home();
        let mut engine = open_engine(&home);
        let source = session_id("fork-child-source");
        let parent = run_id("fork-child-parent");
        let child = child_task_id("fork-child-same-id");
        seed_session_and_run(&mut engine, source.as_str(), parent.as_str());
        begin_parent_exchange(
            &mut engine,
            &source,
            &parent,
            "fork-delegate",
            "fork-child-receipt",
        );
        let source_images = engine
            .session_directory(&source)
            .unwrap()
            .join("tool-images");
        let image_bytes = png_bytes(home.path(), "fork-child.png", [12, 34, 56]);
        let image = crate::image::store_tool_image_bytes(&source_images, &image_bytes).unwrap();
        engine
            .create_child_task(NewStoredChildTask {
                context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                context_window_tokens: 8192,
                child_task_id: child.clone(),
                session_id: source.clone(),
                parent_run_id: parent.clone(),
                parent_tool_call_id: assistant_protocol::ToolCallId::new("fork-delegate").unwrap(),
                title: "inherited child".to_owned(),
                system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
                agent_variant: assistant_protocol::AgentVariant::Build,
                created_at_ms: 2000,
            })
            .unwrap();
        let mut messages = tool_image_exchange(image.clone()).messages.into_iter();
        let ConversationMessage::User(user) = messages.next().unwrap() else {
            unreachable!()
        };
        engine
            .start_child_task(ChildTaskStart {
                operation_id: "fork-child-start".to_owned(),
                child_task_id: child.clone(),
                session_id: source.clone(),
                message: user,
                started_at_ms: 2100,
            })
            .unwrap();
        engine
            .settle_child_task(StoredChildTaskSettlement {
                operation_id: "fork-child-end".to_owned(),
                child_task_id: child.clone(),
                session_id: source.clone(),
                status: ChildTaskStatus::Completed,
                cancel_requested: false,
                error: None,
                messages: messages
                    .chain([assistant_message("fork-child-final", "child result")])
                    .collect(),
                final_message_id: Some(MessageId::new("fork-child-final").unwrap()),
                finished_at_ms: 2200,
            })
            .unwrap();
        complete_parent_exchange(
            &mut engine,
            &source,
            &parent,
            "fork-delegate",
            "fork-child-receipt",
            &child,
            "child result",
        )
        .unwrap();
        finish_fixture_run(
            &mut engine,
            source.as_str(),
            parent.as_str(),
            RunStatus::Completed,
            None,
            2400,
        );
        let child_body = engine.load_child_conversation(&source, &child).unwrap();
        if missing == "body" {
            fs::remove_file(engine.child_body(&source, &child, 1).unwrap()).unwrap();
        } else if missing == "image" {
            fs::remove_file(source_images.join(image.relative_path())).unwrap();
        }
        let mut previous = source.clone();
        let mut targets = Vec::new();
        for name in ["fork-child-target", "fork-child-repeat"] {
            let target = session_id(name);
            let conversation = engine.load_conversation(&previous).unwrap();
            let directories = engine.sessions_directory.clone();
            engine
                .fork_session(SessionFork {
                    source_session_id: previous.clone(),
                    source_generation: 1,
                    session: new_session(target.as_str(), &directories),
                    conversation,
                    attachments: Vec::new(),
                    tool_images: Vec::new(),
                    skill_activations: Vec::new(),
                    mcp_selections: Vec::new(),
                    session_commands: Vec::new(),
                    work_plan: None,
                    goal: None,
                })
                .unwrap();
            let facts = engine
                .load_conversation_history(ConversationOwner::MainSession {
                    session_id: target.clone(),
                })
                .unwrap()
                .facts;
            assert_eq!(facts.child_tasks.len(), 1);
            assert!(facts.child_tasks[0].inherited);
            assert_eq!(facts.child_tasks[0].body_unavailable, missing == "body");
            assert_eq!(
                facts.child_tasks[0].message_count,
                child_body.messages.len() as u64
            );
            for table in ["runs", "inputs", "child_tasks"] {
                assert_eq!(
                    engine
                        .connection
                        .query_row(
                            &format!("SELECT COUNT(*) FROM {table} WHERE session_id=?1"),
                            [target.as_str()],
                            |row| row.get::<_, i64>(0)
                        )
                        .unwrap(),
                    0
                );
            }
            if missing == "body" {
                assert_eq!(
                    engine
                        .load_child_conversation(&target, &child)
                        .unwrap_err()
                        .kind(),
                    StoreErrorKind::ResourceUnavailable
                );
                assert!(
                    !engine.child_body(&target, &child, 1).unwrap().exists(),
                    "never fabricate an empty body"
                );
            } else {
                assert_eq!(
                    engine.load_child_conversation(&target, &child).unwrap(),
                    child_body
                );
                let images = engine
                    .session_directory(&target)
                    .unwrap()
                    .join("tool-images");
                if missing == "image" {
                    assert!(crate::image::validate_tool_image_file(&images, &image).is_err());
                } else {
                    assert_eq!(
                        fs::read(images.join(image.relative_path())).unwrap(),
                        image_bytes
                    );
                }
            }
            // 即便继承正文不可用、没有物理目录，也不能把同名 child 变成新执行。
            let suffix = format!("{name}-local-parent");
            write_fixture_turn(&mut engine, &target, &suffix, 3000, false);
            let new_parent = run_id(&format!("run-{suffix}"));
            assert_eq!(
                engine
                    .create_child_task(NewStoredChildTask {
                        context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                        context_window_tokens: 8192,
                        child_task_id: child.clone(),
                        session_id: target.clone(),
                        parent_run_id: new_parent.clone(),
                        parent_tool_call_id: assistant_protocol::ToolCallId::new("new-local-call")
                            .unwrap(),
                        title: "must not replace inherited child".to_owned(),
                        system_prompt: SystemPromptSnapshot::new(vec!["local child".to_owned()]),
                        agent_variant: assistant_protocol::AgentVariant::Build,
                        created_at_ms: 3001,
                    })
                    .unwrap_err()
                    .kind(),
                StoreErrorKind::Conflict
            );
            finish_fixture_run(
                &mut engine,
                target.as_str(),
                new_parent.as_str(),
                RunStatus::Completed,
                None,
                3100,
            );
            previous = target.clone();
            targets.push(target);
        }
        for deleted in [&source, &targets[0]] {
            let impact = engine.inspect_session_deletion(deleted).unwrap();
            engine
                .delete_session(SessionDeletion {
                    session_id: deleted.clone(),
                    operation_id: format!("delete-{deleted}"),
                    expected_impact: impact,
                })
                .unwrap();
        }
        drop(engine);
        let mut engine = open_engine(&home);
        let target = &targets[1];
        engine.load_session_state(target).unwrap();
        let facts = engine
            .load_conversation_history(ConversationOwner::MainSession {
                session_id: target.clone(),
            })
            .unwrap()
            .facts;
        assert_eq!(facts.child_tasks[0].body_unavailable, missing == "body");
        if missing != "body" {
            assert_eq!(
                engine.load_child_conversation(target, &child).unwrap(),
                child_body
            );
        }
    }
}

#[test]
fn materialization_identity_never_moves_to_later_inputs_after_clear_or_rewrite() {
    for rewrite in [false, true] {
        let home = home();
        let mut engine = open_engine(&home);
        let session = session_id("materialization-original-owner");
        let key = IdempotencyKey::new("materialization-original-key").unwrap();
        let mut created = new_session(session.as_str(), &engine.sessions_directory);
        created.materialization_key = Some(key.clone());
        let original = NewStoredSessionMaterialization {
            session: created.clone(),
            attachments: Vec::new(),
            input: NewStoredInput {
                feedback_answer: None,
                agent_shell_target: None,
                input_id: InputId::new("original-materialized-input").unwrap(),
                session_id: session.clone(),
                idempotency_key: None,
                agent_variant: assistant_protocol::AgentVariant::Build,
                origin: InputOrigin::User,
                goal_binding: None,
                cross_session: None,
                channel_source: Some(desktop_channel_source()),
                skill_activation: None,
                mcp_selection: None,
                message: raw_user_message("original-materialized-user", "original input"),
                new_goal: None,
                resumed_goal: None,
                generated_title: None,
                accepted_at_ms: 1000,
            },
        };
        engine.materialize_session(original.clone()).unwrap();
        claim_fixture(&mut engine, &original.input);
        engine
            .commit_user_message(UserMessageCommit {
                operation_id: "original-start".into(),
                input_id: original.input.input_id.clone(),
                run_id: fixture_run_id(&original.input).clone(),
                session_id: session.clone(),
                message: Some(original.input.message.clone()),
                context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                shell: None,
                reasoning_effort: None,
                created_at_ms: 1100,
            })
            .unwrap();
        engine
            .settle_run(StoredRunSettlement {
                operation_id: "original-finish".into(),
                run_id: fixture_run_id(&original.input).clone(),
                session_id: session.clone(),
                status: RunStatus::Completed,
                cancel_requested: false,
                error: None,
                queued_user_message: None,
                messages: Vec::new(),
                message_step: None,
                goal_effect: None,
                proxy_report: None,
                finished_at_ms: 1200,
            })
            .unwrap();
        assert_eq!(
            engine
                .get_session_materialization(&key)
                .unwrap()
                .unwrap()
                .accepted
                .input_id(),
            &original.input.input_id
        );
        let mut later = original.input.clone();
        later.input_id = InputId::new("later-materialized-input").unwrap();

        later.message = raw_user_message("later-materialized-user", "later input");
        // 即使时钟与原接纳相同，也不能用时间或当前排序推断原身份。
        if rewrite {
            engine
                .rewrite_from_user(ConversationRewrite {
                    run_id: fixture_run_id(&later.clone()),
                    approval_mode: assistant_protocol::ApprovalMode::Ask,
                    session_id: session.clone(),
                    target_user_message_id: original.input.message.id.clone(),
                    conversation: ConversationSnapshot::new(vec![ConversationMessage::User(
                        later.message.clone(),
                    )]),
                    input: later.clone(),
                    goal_effect: None,
                    changed_at_ms: 1300,
                })
                .unwrap();
        } else {
            engine
                .clear_session_history(SessionHistoryClear {
                    operation_id: IdempotencyKey::new("original-clear").unwrap(),
                    session_id: session.clone(),
                    expected_generation: 1,
                    system_prompt: created.system_prompt,
                    environment: created.environment,
                    expected_role: SessionRole::Standard,
                    changed_at_ms: 1300,
                })
                .unwrap();
            engine.accept_input(later.clone()).unwrap();
            // 不应为已移除的原身份解码当前队列，损坏的另一条载荷也不能掩盖过期判断。
            engine
                .connection
                .execute(
                    "UPDATE inputs SET queued_message_json='{}' WHERE input_id=?1",
                    [later.input_id.as_str()],
                )
                .unwrap();
        }
        let binding: String = engine
            .connection
            .query_row(
                "SELECT materialization_input_id FROM sessions WHERE session_id=?1",
                [session.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding, original.input.input_id.as_str());
        assert_eq!(
            engine
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM inputs WHERE input_id=?1",
                    [original.input.input_id.as_str()],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let generation: i64 = engine
            .connection
            .query_row(
                "SELECT body_generation FROM sessions WHERE session_id=?1",
                [session.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let body = body_path(
            &engine.sessions_directory.join(session.as_str()),
            generation as u64,
        );
        let bytes = fs::read(&body).unwrap();
        for attempt in 0..2 {
            if attempt == 1 {
                drop(engine);
                engine = StorageEngine::open(home.path()).unwrap();
            }
            let changes = engine.connection.total_changes();
            assert_eq!(
                engine.get_session_materialization(&key).unwrap_err().kind(),
                StoreErrorKind::Conflict
            );
            assert_eq!(
                engine
                    .materialize_session(original.clone())
                    .unwrap_err()
                    .kind(),
                StoreErrorKind::Conflict
            );
            assert_eq!(engine.connection.total_changes(), changes);
            for (table, expected) in [
                ("sessions", 1),
                ("inputs", 1),
                ("runs", if rewrite { 1 } else { 0 }),
                ("body_appends", 0),
            ] {
                assert_eq!(
                    engine
                        .connection
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                            .get::<_, i64>(0))
                        .unwrap(),
                    expected
                );
            }
            assert_eq!(fs::read(&body).unwrap(), bytes);
        }
    }
}

#[tokio::test]
async fn accepted_and_materialized_retries_read_terminal_files_without_decoding_other_queue_payloads()
 {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("accepted-file-owner");
    let mut created = new_session(session.as_str(), &engine.sessions_directory);
    let key = IdempotencyKey::new("file-materialization-key").unwrap();
    created.materialization_key = Some(key.clone());
    let original = NewStoredSessionMaterialization {
        session: created,
        attachments: Vec::new(),
        input: NewStoredInput {
            feedback_answer: None,
            agent_shell_target: None,
            input_id: InputId::new("accepted-file-input").unwrap(),
            session_id: session.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: InputOrigin::User,
            goal_binding: None,
            cross_session: None,
            channel_source: Some(desktop_channel_source()),
            skill_activation: None,
            mcp_selection: None,
            message: raw_user_message("accepted-file-user", "original input"),
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 1000,
        },
    };
    engine.materialize_session(original.clone()).unwrap();
    claim_fixture(&mut engine, &original.input);
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "accepted-file-start".into(),
            input_id: original.input.input_id.clone(),
            run_id: fixture_run_id(&original.input).clone(),
            session_id: session.clone(),
            message: Some(original.input.message.clone()),
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            reasoning_effort: None,
            created_at_ms: 1500,
        })
        .unwrap();
    engine
        .settle_run(StoredRunSettlement {
            operation_id: "accepted-file-end".into(),
            run_id: fixture_run_id(&original.input).clone(),
            session_id: session.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            queued_user_message: None,
            messages: vec![assistant_message("accepted-file-final", "reliable final")],
            message_step: Some(1),
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: 2200,
        })
        .unwrap();
    // 物化 key 属于 Session；普通输入使用自己的 key，分别核对首次 Run。
    let mut accepted_input = original.input.clone();
    accepted_input.input_id = InputId::new("accepted-next-input").unwrap();

    accepted_input.idempotency_key = Some(IdempotencyKey::new("accepted-next-key").unwrap());
    accepted_input.message = raw_user_message("accepted-next-user", "normal input");
    accepted_input.accepted_at_ms = 2250;
    engine.accept_input(accepted_input.clone()).unwrap();
    claim_fixture(&mut engine, &accepted_input);
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "accepted-next-start".into(),
            input_id: accepted_input.input_id.clone(),
            run_id: fixture_run_id(&accepted_input).clone(),
            session_id: session.clone(),
            message: Some(accepted_input.message.clone()),
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            reasoning_effort: None,
            created_at_ms: 2260,
        })
        .unwrap();
    engine
        .settle_run(StoredRunSettlement {
            operation_id: "accepted-next-end".into(),
            run_id: fixture_run_id(&accepted_input).clone(),
            session_id: session.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            queued_user_message: None,
            messages: vec![assistant_message("accepted-next-final", "normal final")],
            message_step: Some(1),
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: 2300,
        })
        .unwrap();
    let mut queued = original.input.clone();
    queued.input_id = InputId::new("unrelated-queued-input").unwrap();

    queued.idempotency_key = Some(IdempotencyKey::new("unrelated-queued-key").unwrap());
    queued.message = raw_user_message("unrelated-queued-user", "waiting input");
    queued.accepted_at_ms = 2400;
    engine.accept_input(queued.clone()).unwrap();
    // 可靠终态已无 SQL 控制；只在此夹具注入旧终态，验证在线读取不使用其失真载荷。
    for input in [&original.input, &accepted_input] {
        engine.connection.execute("INSERT INTO runs(run_id,session_id,input_id,attempt,status,cancel_requested,created_at_ms,started_at_ms,shell_snapshot_json) VALUES(?1,?2,?3,1,'completed',0,999,999999,'{}')",
            params![fixture_run_id(input).as_str(), session.as_str(), input.input_id.as_str()]).unwrap();
    }
    // 只污染隔离夹具：幂等读取不得解码过期执行载荷，也不能读取/清空另一条队列正文。
    assert_eq!(engine.connection.execute("UPDATE runs SET shell_snapshot_json='{}',started_at_ms=999999 WHERE session_id=?1 AND status='completed'", [session.as_str()]).unwrap(), 2);
    let queued_json: String = engine
        .connection
        .query_row(
            "SELECT queued_message_json FROM inputs WHERE input_id=?1",
            [queued.input_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        engine
            .connection
            .execute(
                "UPDATE inputs SET queued_message_json='{}' WHERE input_id=?1",
                [queued.input_id.as_str()]
            )
            .unwrap(),
        1
    );
    let path = body_path(&engine.session_directory(&session).unwrap(), 1);
    let body = fs::read(&path).unwrap();
    for remove_control in [false, true] {
        if remove_control {
            assert_eq!(
                engine
                    .connection
                    .execute(
                        "DELETE FROM runs WHERE session_id=?1 AND status='completed'",
                        [session.as_str()]
                    )
                    .unwrap(),
                2
            );
        }
        for accepted in [
            engine.accept_input(accepted_input.clone()).unwrap(),
            engine
                .get_session_materialization(&key)
                .unwrap()
                .unwrap()
                .accepted,
            engine
                .materialize_session(original.clone())
                .unwrap()
                .accepted,
        ] {
            let assistant_runtime::AcceptedInput::Historical {
                session_id,
                input,
                run,
            } = accepted
            else {
                panic!("terminal idempotency must not return execution control");
            };
            assert_eq!(session_id, session);
            assert_eq!(run.status, Some(RunStatus::Completed));
            if input.input_id == original.input.input_id {
                assert_eq!(run.run_id, fixture_run_id(&original.input));
                assert_eq!(run.started_at_ms, Some(1500));
                assert_eq!(run.finished_at_ms, Some(2200));
            } else {
                assert_eq!(input.input_id, accepted_input.input_id);
                assert_eq!(run.run_id, fixture_run_id(&accepted_input));
                assert_eq!(run.started_at_ms, Some(2260));
                assert_eq!(run.finished_at_ms, Some(2300));
            }
        }
        assert_eq!(fs::read(&path).unwrap(), body);
        let retained: (String, String) = engine
            .connection
            .query_row(
                "SELECT state,queued_message_json FROM inputs WHERE input_id=?1",
                [queued.input_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(retained, ("queued".into(), "{}".into()));
    }
    assert_eq!(
        engine
            .connection
            .execute(
                "UPDATE inputs SET queued_message_json=?1 WHERE input_id=?2",
                params![queued_json, queued.input_id.as_str()]
            )
            .unwrap(),
        1
    );
    engine
        .cancel_queued_input(&session, &queued.input_id)
        .unwrap();
    drop(engine);
    let store = std::sync::Arc::new(LocalRuntimeStore::open(home.path(), 32).await.unwrap());
    let resources = crate::resources::HostResources::new(home.path()).unwrap();
    let runtime = assistant_runtime::AssistantRuntime::open_with_recall_key(
        assistant_runtime::RuntimeConfig::new(std::num::NonZeroUsize::new(32).unwrap()),
        std::sync::Arc::new(crate::config_source::LocalConfigSource::new(
            home.path().join("config.toml"),
        )),
        resources.model_factory,
        resources.session_environment_factory,
        resources.skill_package_source,
        resources.run_tool_factory,
        resources.child_task_workspace_factory,
        store.clone(),
        store.clone(),
        [32; 32],
    )
    .await
    .unwrap();
    let result = runtime
        .get_session_materialization(assistant_protocol::GetSessionMaterializationRequest {
            idempotency_key: key,
        })
        .await
        .unwrap()
        .materialization
        .unwrap();
    assert_eq!(result.input_id, original.input.input_id);
    assert_eq!(
        result.run.as_ref().unwrap().run_id,
        fixture_run_id(&original.input)
    );
    assert_eq!(result.run.as_ref().unwrap().status, RunStatus::Completed);
    assert_eq!(result.run.as_ref().unwrap().text, "reliable final");
    assert!(
        store
            .load_session_state(&session)
            .await
            .unwrap()
            .state
            .runs
            .is_empty()
    );
    assert_eq!(fs::read(path).unwrap(), body);
    runtime
        .shutdown(assistant_protocol::ShutdownRuntimeRequest {})
        .await
        .unwrap();
}

fn begin_parent_exchange(
    engine: &mut StorageEngine,
    session: &SessionId,
    run: &RunId,
    call: &str,
    receipt: &str,
) {
    assert_eq!(
        engine
            .connection
            .execute(
                "UPDATE runs SET status='running',started_at_ms=1500 WHERE run_id=?1",
                [run.as_str()]
            )
            .unwrap(),
        1
    );
    let mut pending = pending_delegate_exchange(session.as_str(), run.as_str(), receipt);
    for part in &mut pending.assistant.parts {
        if let AssistantPart::ToolCall(tool) = part {
            tool.id = ToolCallId::new(call).unwrap();
        }
    }
    engine.begin_tool_exchange(pending).unwrap();
    engine
        .mark_tool_execution_started(ToolExecutionStart {
            receipt: ExchangeReceipt::new(receipt).unwrap(),
            session_id: session.clone(),
            run_id: run.clone(),
            call_id: assistant_protocol::ToolCallId::new(call).unwrap(),
            started_at_ms: 2001,
        })
        .unwrap();
}

fn complete_parent_exchange(
    engine: &mut StorageEngine,
    session: &SessionId,
    run: &RunId,
    call: &str,
    receipt: &str,
    child: &ChildTaskId,
    text: &str,
) -> Result<(), assistant_runtime::StoreError> {
    let mut results = tool_results();
    results[0].id = MessageId::new(format!("result-{receipt}")).unwrap();
    results[0].result.call_id = ToolCallId::new(call).unwrap();
    results[0].result.content = ToolResultContent::json(
        serde_json::json!({"task_id":child.as_str(),"status":"completed","result":text}),
    );
    engine.complete_tool_exchange(CompletedToolExchange {
        operation_id: format!("closed-{receipt}"),
        receipt: ExchangeReceipt::new(receipt).unwrap(),
        session_id: session.clone(),
        run_id: run.clone(),
        step: 1,
        results,
        activation_message: None,
        skill_activations: Vec::new(),
        completed_at_ms: 2300,
    })
}

fn pending_child_fixture() -> (TempDir, StorageEngine, SessionId, RunId, ChildTaskId) {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("pending-child-owner");
    let run = run_id("pending-child-parent");
    let child = child_task_id("pending-child-one");
    seed_session_and_run(&mut engine, session.as_str(), run.as_str());
    assert_eq!(
        engine
            .connection
            .execute(
                "UPDATE runs SET status='running',started_at_ms=1500 WHERE run_id=?1",
                [run.as_str()]
            )
            .unwrap(),
        1
    );
    engine
        .begin_tool_exchange(pending_delegate_exchange(
            session.as_str(),
            run.as_str(),
            "pending-child-receipt",
        ))
        .unwrap();
    start_tool(
        &mut engine,
        session.as_str(),
        run.as_str(),
        "pending-child-receipt",
    );
    engine
        .create_child_task(NewStoredChildTask {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            context_window_tokens: 8192,
            child_task_id: child.clone(),
            session_id: session.clone(),
            parent_run_id: run.clone(),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("call-1").unwrap(),
            title: "pending child".into(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".into()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .unwrap();
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "pending-child-start".into(),
            child_task_id: child.clone(),
            session_id: session.clone(),
            message: raw_user_message("pending-child-user", "inspect"),
            started_at_ms: 2_100,
        })
        .unwrap();
    (home, engine, session, run, child)
}

#[test]
fn parent_history_freezes_child_file_after_its_terminal_control_was_released() {
    let (_home, mut engine, session, run, child) = pending_child_fixture();
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "freeze-child-end".into(),
            child_task_id: child.clone(),
            session_id: session.clone(),
            status: ChildTaskStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("freeze-final", "file answer")],
            final_message_id: Some(MessageId::new("freeze-final").unwrap()),
            finished_at_ms: 2200,
        })
        .unwrap();
    // child 自己完成可靠提交即释放控制，不等待父交换；父帧只能通过回执读取文件。
    assert!(
        engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        engine.load_child_history_catalog(&session).unwrap()[0].finished_at_ms,
        Some(2200)
    );
    assert_eq!(
        engine
            .inspect_session_deletion(&session)
            .unwrap()
            .child_task_count,
        1
    );
    assert_eq!(
        engine
            .load_child_conversation(&session, &child)
            .unwrap()
            .messages
            .len(),
        2
    );
    complete_parent_exchange(
        &mut engine,
        &session,
        &run,
        "call-1",
        "pending-child-receipt",
        &child,
        "file answer",
    )
    .unwrap();
    let history = engine
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: session.clone(),
        })
        .unwrap()
        .facts;
    assert_eq!(history.child_tasks.len(), 1);
    assert_eq!(history.child_tasks[0].child_task_id, child);
    assert_eq!(
        history.child_tasks[0].status,
        Some(ChildTaskStatus::Completed)
    );
    assert_eq!(history.child_tasks[0].finished_at_ms, Some(2200));
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM child_tasks WHERE child_task_id=?1",
                [child.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .load_child_conversation(&session, &child)
            .unwrap()
            .messages
            .len(),
        2
    );
}

#[test]
fn normal_child_reads_ignore_stale_terminal_sql_pointers_and_never_decode_their_payload() {
    let (home, mut engine, session, run, child) = pending_child_fixture();
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "stale-child-end".into(),
            child_task_id: child.clone(),
            session_id: session.clone(),
            status: ChildTaskStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("stale-child-final", "file truth")],
            final_message_id: Some(MessageId::new("stale-child-final").unwrap()),
            finished_at_ms: 2200,
        })
        .unwrap();
    assert!(
        engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .is_none()
    );
    // 只向本次隔离夹具注入过期且正文不可解码的旧终态行；普通读/控制不能再消费它。
    assert_eq!(engine.connection.execute(
        "INSERT INTO child_tasks(child_task_id,session_id,parent_run_id,parent_tool_call_id,title,system_prompt_json,agent_variant,status,cancel_requested,body_generation,message_count,created_at_ms,started_at_ms)
         VALUES(?1,?2,?3,'call-1','stale SQL title','{}','build','completed',0,99,999,1,9999)",
        params![child.as_str(),session.as_str(),run.as_str()]).unwrap(),1);
    assert!(engine.load_child_tasks().unwrap().is_empty());
    assert!(
        engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .is_none()
    );
    assert!(engine.child_generation(&session, &child).is_err());
    assert_eq!(
        engine
            .request_child_task_cancellation(&session, &child)
            .unwrap_err()
            .kind(),
        StoreErrorKind::Conflict
    );
    assert_eq!(
        engine.child_history_generation(&session, &child).unwrap(),
        1
    );
    assert_eq!(
        engine
            .load_child_conversation(&session, &child)
            .unwrap()
            .messages
            .len(),
        2
    );
    let catalog = engine.load_child_history_catalog(&session).unwrap();
    assert_eq!(catalog[0].finished_at_ms, Some(2200));
    assert_eq!(catalog[0].title, "pending child");
    assert!(engine.recover_tool_images().unwrap().is_empty());
    assert_eq!(
        engine
            .inspect_session_deletion(&session)
            .unwrap()
            .child_task_count,
        1
    );
    // 已对齐库在下一次准入会拒绝这份人为污染，不把违规终态当作可恢复控制。
    assert!(StorageEngine::open(home.path()).is_err());
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM child_tasks WHERE body_generation=99",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        engine
            .connection
            .execute(
                "DELETE FROM child_tasks WHERE child_task_id=?1",
                [child.as_str()]
            )
            .unwrap(),
        1
    );
    complete_parent_exchange(
        &mut engine,
        &session,
        &run,
        "call-1",
        "pending-child-receipt",
        &child,
        "file truth",
    )
    .unwrap();
    assert_eq!(
        engine.load_child_history_catalog(&session).unwrap(),
        catalog
    );
}

#[test]
fn child_control_release_failure_retains_its_receipt_and_parent_pending_until_restart() {
    let (home, mut engine, session, _run, child) = pending_child_fixture();
    let main_path = body_path(&engine.session_directory(&session).unwrap(), 1);
    let parent_before = fs::read(&main_path).unwrap();
    // 删除发生在 child 原回执删除之后；故障必须回滚位置、控制状态与回执的整个完成事务。
    engine.connection.execute_batch("CREATE TRIGGER pause_child_release BEFORE DELETE ON child_tasks BEGIN SELECT RAISE(ABORT,'injected child release failure'); END;").unwrap();
    assert!(
        engine
            .settle_child_task(StoredChildTaskSettlement {
                operation_id: "release-child-end".into(),
                child_task_id: child.clone(),
                session_id: session.clone(),
                status: ChildTaskStatus::Completed,
                cancel_requested: false,
                error: None,
                messages: vec![assistant_message("release-final", "file answer")],
                final_message_id: Some(MessageId::new("release-final").unwrap()),
                finished_at_ms: 2200,
            })
            .is_err()
    );
    for table in [
        "child_tasks",
        "pending_tool_exchanges",
        "pending_tool_starts",
        "child_body_appends",
    ] {
        assert_eq!(
            engine
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1,
            "{table}"
        );
    }
    assert_eq!(
        engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .unwrap()
            .status,
        ChildTaskStatus::Running
    );
    assert!(
        engine
            .connection
            .query_row(
                "SELECT delegated_child_json FROM pending_tool_starts",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap()
            .is_none()
    );
    assert!(engine.load_child_conversation(&session, &child).is_err());
    assert_eq!(fs::read(&main_path).unwrap(), parent_before);
    let child_path = engine.child_body(&session, &child, 1).unwrap();
    let written = fs::read(&child_path).unwrap();
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_child_release")
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    let recovered = engine.load_runtime().unwrap();
    assert!(recovered.child_tasks.is_empty());
    assert_eq!(recovered.child_history[0].1.child_task_id, child);
    for table in [
        "child_tasks",
        "pending_tool_exchanges",
        "pending_tool_starts",
        "body_appends",
        "child_body_appends",
    ] {
        assert_eq!(
            engine
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(fs::read(child_path).unwrap(), written);
    assert_eq!(
        engine.load_conversation(&session).unwrap().messages.len(),
        2
    );
    assert_eq!(
        engine
            .load_child_conversation(&session, &child)
            .unwrap()
            .messages
            .len(),
        2
    );
    let history = engine.load_child_history_catalog(&session).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].finished_at_ms, Some(2200));
}

#[test]
fn pending_child_location_recovers_both_receipts_without_terminal_sql_or_early_publication() {
    let (home, mut engine, session, _, child) = pending_child_fixture();
    let main_path = body_path(&engine.session_directory(&session).unwrap(), 1);
    let before = fs::read(&main_path).unwrap();
    engine.connection.execute_batch("CREATE TRIGGER pause_child_completion BEFORE DELETE ON child_body_appends BEGIN SELECT RAISE(ABORT,'injected child completion failure'); END;").unwrap();
    assert!(
        engine
            .settle_child_task(StoredChildTaskSettlement {
                operation_id: "pending-child-end".into(),
                child_task_id: child.clone(),
                session_id: session.clone(),
                status: ChildTaskStatus::Completed,
                cancel_requested: false,
                error: None,
                messages: vec![assistant_message("pending-child-final", "recovered answer")],
                final_message_id: Some(MessageId::new("pending-child-final").unwrap()),
                finished_at_ms: 2_200,
            })
            .is_err()
    );
    assert!(
        engine
            .connection
            .query_row(
                "SELECT delegated_child_json FROM pending_tool_starts",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .unwrap()
            .status,
        ChildTaskStatus::Running
    );
    assert!(
        engine
            .recover_pending_tool_exchanges()
            .unwrap()
            .contains(session.as_str())
    );
    assert_eq!(
        fs::read(&main_path).unwrap(),
        before,
        "parent cannot publish unknown before child receipt is reliable"
    );
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_child_completion")
        .unwrap();
    assert!(engine.recover_child_storage().unwrap().is_empty());
    let location: String = engine
        .connection
        .query_row(
            "SELECT delegated_child_json FROM pending_tool_starts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&location).unwrap();
    assert_eq!(
        json.as_object().unwrap().len(),
        2,
        "pending row stores only the file location"
    );
    assert_eq!(json["child_task_id"], child.as_str());
    assert_eq!(json["body_generation"], 1);
    assert!(
        engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .is_none()
    );
    // 只污染本次夹具的文件位置；终态 SQL 已由正式提交释放，位置损坏必须明确失败。
    assert_eq!(engine.connection.execute("UPDATE pending_tool_starts SET delegated_child_json=json_set(delegated_child_json,'$.body_generation',99)",[]).unwrap(),1);
    assert!(
        engine
            .recover_pending_tool_exchanges()
            .unwrap()
            .contains(session.as_str())
    );
    assert_eq!(fs::read(&main_path).unwrap(), before);
    assert_eq!(
        engine
            .connection
            .execute(
                "UPDATE pending_tool_starts SET delegated_child_json=?1",
                [location]
            )
            .unwrap(),
        1
    );
    assert_eq!(
        engine
            .connection
            .execute(
                "DELETE FROM child_tasks WHERE child_task_id=?1",
                [child.as_str()]
            )
            .unwrap(),
        0
    );
    engine.connection.execute_batch("CREATE TRIGGER pause_parent_completion BEFORE DELETE ON body_appends BEGIN SELECT RAISE(ABORT,'injected parent completion failure'); END;").unwrap();
    assert!(
        engine
            .recover_pending_tool_exchanges()
            .unwrap()
            .contains(session.as_str())
    );
    assert!(engine.load_conversation(&session).is_err());
    assert_eq!(
        engine
            .connection
            .query_row("SELECT state FROM pending_tool_exchanges", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "ready"
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pending_tool_starts WHERE delegated_child_json IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_parent_completion")
        .unwrap();
    drop(engine);

    let mut engine = open_engine(&home);
    let recovered = engine.load_runtime().unwrap();
    assert!(recovered.child_tasks.is_empty());
    assert_eq!(recovered.child_history[0].1.child_task_id, child);
    assert_eq!(
        recovered.child_history[0].1.status,
        Some(ChildTaskStatus::Completed)
    );
    let body = engine.load_conversation(&session).unwrap();
    let ConversationMessage::Tool(tool) = &body.messages[1] else {
        unreachable!()
    };
    assert_eq!(tool.result.status, ToolResultStatus::Success);
    assert_eq!(
        tool.result.content.as_single_json().unwrap()["result"],
        "recovered answer"
    );
    assert_eq!(
        engine
            .load_child_conversation(&session, &child)
            .unwrap()
            .messages
            .len(),
        2
    );
    for table in [
        "child_tasks",
        "body_appends",
        "child_body_appends",
        "pending_tool_exchanges",
        "pending_tool_starts",
    ] {
        assert_eq!(
            engine
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    let committed = fs::read(&main_path).unwrap();
    engine.load_runtime().unwrap();
    assert_eq!(fs::read(main_path).unwrap(), committed);
}

#[test]
fn file_only_children_reuse_ids_without_crossing_recall_owners_or_titles() {
    let home = home();
    let mut engine = open_engine(&home);
    let child = child_task_id("same-child-id");
    for index in 0..2 {
        let session = session_id(&format!("recall-file-{index}"));
        let parent = run_id(&format!("recall-parent-{index}"));
        seed_session_and_run(&mut engine, session.as_str(), parent.as_str());
        let receipt = format!("pending-parent-{index}");
        begin_parent_exchange(&mut engine, &session, &parent, "same-call", &receipt);
        engine
            .create_child_task(NewStoredChildTask {
                context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                context_window_tokens: 8192,
                child_task_id: child.clone(),
                session_id: session.clone(),
                parent_run_id: parent.clone(),
                parent_tool_call_id: assistant_protocol::ToolCallId::new("same-call").unwrap(),
                title: format!("owner title {index}"),
                system_prompt: SystemPromptSnapshot::new(vec!["child".into()]),
                agent_variant: assistant_protocol::AgentVariant::Build,
                created_at_ms: 2_000,
            })
            .unwrap();
        engine
            .start_child_task(ChildTaskStart {
                operation_id: "same-start".into(),
                child_task_id: child.clone(),
                session_id: session.clone(),
                message: raw_user_message("same-user", "inspect"),
                started_at_ms: 2_100,
            })
            .unwrap();
        engine
            .settle_child_task(StoredChildTaskSettlement {
                operation_id: "same-end".into(),
                child_task_id: child.clone(),
                session_id: session.clone(),
                status: ChildTaskStatus::Completed,
                cancel_requested: false,
                error: None,
                messages: vec![assistant_message(
                    "same-answer",
                    &format!("owner-recall-marker {index}"),
                )],
                final_message_id: Some(MessageId::new("same-answer").unwrap()),
                finished_at_ms: 2_200,
            })
            .unwrap();
        // 下一会话复用同一历史 child ID；SQL 中不保留前一会话终态关系。
        assert_eq!(
            engine
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM child_tasks WHERE child_task_id=?1",
                    [child.as_str()],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let request = ConversationSearchRequest {
        query: "owner-recall-marker".into(),
        scope: ConversationSearchScope::Global,
        limit: 10,
    };
    let titles = engine
        .search_conversation_titles(ConversationSearchRequest {
            query: "owner title".into(),
            ..request.clone()
        })
        .unwrap();
    assert_eq!(
        titles.hits.len(),
        2,
        "released children remain searchable by their file titles"
    );
    assert!(!titles.partial);
    let scoped_titles = engine
        .search_conversation_titles(ConversationSearchRequest {
            query: "OWNER TITLE".into(),
            scope: ConversationSearchScope::Session {
                session_id: session_id("recall-file-1"),
            },
            limit: 1,
        })
        .unwrap();
    assert_eq!(scoped_titles.hits.len(), 1);
    assert_eq!(
        scoped_titles.hits[0].child_task_title.as_deref(),
        Some("owner title 1")
    );
    let page = engine.search_conversations(request.clone()).unwrap();
    assert!(!page.partial);
    assert_eq!(page.hits.len(), 2);
    for index in 0..2 {
        let hit = page
            .hits
            .iter()
            .find(|hit| {
                hit.owner
                    == ConversationOwner::ChildTask {
                        session_id: session_id(&format!("recall-file-{index}")),
                        child_task_id: child.clone(),
                    }
            })
            .unwrap();
        assert_eq!(hit.message_id.as_str(), "same-answer");
        assert_eq!(
            hit.child_task_title.as_deref(),
            Some(format!("owner title {index}").as_str())
        );
        assert_eq!(hit.text, format!("owner-recall-marker {index}"));
    }
    // 两个会话的同 ID child 仍在各自父 pending 中；随后可靠父帧接替位置，结果保持。
    for index in 0..2 {
        complete_parent_exchange(
            &mut engine,
            &session_id(&format!("recall-file-{index}")),
            &run_id(&format!("recall-parent-{index}")),
            "same-call",
            &format!("pending-parent-{index}"),
            &child,
            &format!("owner-recall-marker {index}"),
        )
        .unwrap();
    }
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM pending_tool_starts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        engine.search_conversations(request.clone()).unwrap().hits,
        page.hits
    );
    // 本次新建可重建夹具清掉派生索引，证明两个 owner 都能从文件独立重建。
    engine
        .connection
        .execute("DELETE FROM conversation_recall_documents", [])
        .unwrap();
    engine
        .connection
        .execute("DELETE FROM conversation_recall_heads", [])
        .unwrap();
    let rebuilt = engine.search_conversations(request.clone()).unwrap();
    assert!(!rebuilt.partial);
    assert_eq!(rebuilt.hits, page.hits);

    // 构造排序在前的陈旧 ready 索引；它不能消耗结果上限或为自己创建历史 owner。
    assert_eq!(engine.connection.execute(
        "INSERT INTO conversation_recall_heads(owner_kind,owner_id,session_id,child_task_id,body_generation,indexed_message_count,state,updated_at_ms)
         SELECT owner_kind,'obsolete-owner',session_id,child_task_id,99,indexed_message_count,'ready',updated_at_ms
         FROM conversation_recall_heads WHERE owner_kind='child_task' AND session_id='recall-file-0'", []
    ).unwrap(), 1);
    assert_eq!(engine.connection.execute(
        "INSERT INTO conversation_recall_documents(document_id,owner_kind,owner_id,session_id,child_task_id,body_generation,message_id,message_kind,message_ordinal,created_at_ms,normalized_text,content_hash)
         SELECT 'obsolete-document',owner_kind,'obsolete-owner',session_id,child_task_id,99,message_id,message_kind,message_ordinal,999999,normalized_text,content_hash
         FROM conversation_recall_documents WHERE owner_kind='child_task' AND session_id='recall-file-0' AND message_id='same-answer'", []
    ).unwrap(), 1);
    let scoped = engine
        .search_conversations(ConversationSearchRequest {
            scope: ConversationSearchScope::Session {
                session_id: session_id("recall-file-0"),
            },
            limit: 1,
            ..request.clone()
        })
        .unwrap();
    assert_eq!(scoped.hits.len(), 1);
    assert_eq!(scoped.hits[0].generation, 1);
    assert_eq!(scoped.hits[0].text, "owner-recall-marker 0");

    // ready 不是读取许可；同 ID 的一个 child 文件损坏后仅隔离它，另一会话仍可搜索。
    fs::write(
        engine
            .child_body(&session_id("recall-file-0"), &child, 1)
            .unwrap(),
        b"invalid",
    )
    .unwrap();
    let partial = engine.search_conversations(request).unwrap();
    assert!(partial.partial);
    assert!(
        partial
            .failed_owners
            .contains(&ConversationOwner::ChildTask {
                session_id: session_id("recall-file-0"),
                child_task_id: child
            })
    );
    assert_eq!(partial.hits.len(), 1);
    assert_eq!(partial.hits[0].text, "owner-recall-marker 1");
    let broken = session_id("recall-file-0");
    fs::write(
        body_path(&engine.session_directory(&broken).unwrap(), 1),
        b"bad history",
    )
    .unwrap();
    let titles = engine
        .search_conversation_titles(ConversationSearchRequest {
            query: "owner title".into(),
            scope: ConversationSearchScope::Global,
            limit: 10,
        })
        .unwrap();
    assert!(titles.partial);
    assert_eq!(
        titles.failed_owners,
        [ConversationOwner::MainSession {
            session_id: broken.clone()
        }]
    );
    assert_eq!(titles.hits.len(), 1);
    assert_eq!(
        titles.hits[0].child_task_title.as_deref(),
        Some("owner title 1")
    );
    assert!(
        engine.inspect_session_deletion(&broken).is_err(),
        "broken file cannot become a guessed zero impact"
    );
    let unaffected = engine
        .search_conversation_titles(ConversationSearchRequest {
            query: "owner title".into(),
            scope: ConversationSearchScope::Session {
                session_id: session_id("recall-file-1"),
            },
            limit: 1,
        })
        .unwrap();
    assert!(!unaffected.partial);
    assert_eq!(unaffected.hits.len(), 1);
}

#[test]
fn missing_recall_trigger_resets_stale_virtual_index_before_lazy_rebuild() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("recall-trigger-owner");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    commit_completed_turn(&mut engine, &session, "recall-trigger-marker", 2_000);
    let request = ConversationSearchRequest {
        query: "recall-trigger-marker".into(),
        scope: ConversationSearchScope::Global,
        limit: 20,
    };
    let before = engine.search_conversations(request.clone()).unwrap();
    assert_eq!(before.hits.len(), 2);
    engine
        .connection
        .execute_batch("DROP TRIGGER conversation_recall_documents_au")
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    assert!(engine.recall_index_available);
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM conversation_recall_documents",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM conversation_recall_heads WHERE state='dirty'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let after = engine.search_conversations(request).unwrap();
    assert!(!after.partial);
    assert_eq!(after.hits.len(), 2);
    for hit in after.hits {
        assert!(before.hits.iter().any(|old| old.owner == hit.owner
            && old.message_id == hit.message_id
            && old.text == hit.text
            && old.generation == hit.generation));
    }
}

#[test]
fn child_file_image_survives_control_release_and_unreadable_child_stops_sweep() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("image-file-owner");
    let parent = run_id("image-file-parent");
    let child = child_task_id("image-file-child");
    seed_session_and_run(&mut engine, session.as_str(), parent.as_str());
    begin_parent_exchange(
        &mut engine,
        &session,
        &parent,
        "delegate-image",
        "image-parent-receipt",
    );
    let images = engine
        .session_directory(&session)
        .unwrap()
        .join("tool-images");
    let retained = crate::image::store_tool_image_bytes(
        &images,
        &png_bytes(home.path(), "retained.png", [10, 20, 30]),
    )
    .unwrap();
    let orphan = crate::image::store_tool_image_bytes(
        &images,
        &png_bytes(home.path(), "orphan.png", [40, 50, 60]),
    )
    .unwrap();
    engine
        .create_child_task(NewStoredChildTask {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            context_window_tokens: 8192,
            child_task_id: child.clone(),
            session_id: session.clone(),
            parent_run_id: parent.clone(),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("delegate-image").unwrap(),
            title: "image history".into(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".into()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .unwrap();
    let mut messages = tool_image_exchange(retained.clone()).messages.into_iter();
    let ConversationMessage::User(user) = messages.next().unwrap() else {
        unreachable!()
    };
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "image-child-start".into(),
            child_task_id: child.clone(),
            session_id: session.clone(),
            message: user,
            started_at_ms: 2_100,
        })
        .unwrap();
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "image-child-end".into(),
            child_task_id: child.clone(),
            session_id: session.clone(),
            status: ChildTaskStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: messages
                .chain([assistant_message("image-child-final", "image ready")])
                .collect(),
            final_message_id: Some(MessageId::new("image-child-final").unwrap()),
            finished_at_ms: 2_200,
        })
        .unwrap();
    // 父交换仍在 started 阶段；仅凭临时位置和 child 文件保留图片、详情与删除影响。
    assert!(
        engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        engine
            .inspect_session_deletion(&session)
            .unwrap()
            .child_task_count,
        1
    );
    assert!(engine.recover_tool_images().unwrap().is_empty());
    assert!(images.join(retained.relative_path()).is_file());
    let location: String=engine.connection.query_row("SELECT delegated_child_json FROM pending_tool_starts WHERE receipt_id='image-parent-receipt'",[],|row|row.get(0)).unwrap();
    let pending_preserved = crate::image::store_tool_image_bytes(
        &images,
        &png_bytes(home.path(), "pending-preserved.png", [90, 80, 70]),
    )
    .unwrap();
    assert_eq!(engine.connection.execute("UPDATE pending_tool_starts SET delegated_child_json=json_set(delegated_child_json,'$.body_generation',99) WHERE receipt_id='image-parent-receipt'",[]).unwrap(),1);
    assert!(engine.load_child_history_catalog(&session).is_err());
    assert!(engine.inspect_session_deletion(&session).is_err());
    assert!(
        engine
            .recover_tool_images()
            .unwrap()
            .contains(session.as_str())
    );
    assert!(images.join(retained.relative_path()).is_file());
    assert!(images.join(pending_preserved.relative_path()).is_file());
    assert_eq!(engine.connection.execute("UPDATE pending_tool_starts SET delegated_child_json=?1 WHERE receipt_id='image-parent-receipt'",[location]).unwrap(),1);
    assert!(engine.recover_tool_images().unwrap().is_empty());
    assert!(!images.join(pending_preserved.relative_path()).exists());
    complete_parent_exchange(
        &mut engine,
        &session,
        &parent,
        "delegate-image",
        "image-parent-receipt",
        &child,
        "image ready",
    )
    .unwrap();
    // 正式父交换已释放终态控制；主文件可靠保存 child 目录，正文与图片保持原件。
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM child_tasks WHERE child_task_id=?1",
                [child.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(engine.recover_tool_images().unwrap().is_empty());
    assert!(images.join(retained.relative_path()).is_file());
    assert!(!images.join(orphan.relative_path()).exists());

    // 目录已知但 child 无法读取时，不能把任何完整图片推定为孤儿并删除。
    let preserved = crate::image::store_tool_image_bytes(
        &images,
        &png_bytes(home.path(), "preserved.png", [70, 80, 90]),
    )
    .unwrap();
    fs::write(
        engine.child_body(&session, &child, 1).unwrap(),
        b"unreadable",
    )
    .unwrap();
    assert!(
        engine
            .recover_tool_images()
            .unwrap()
            .contains(session.as_str())
    );
    assert!(images.join(retained.relative_path()).is_file());
    assert!(images.join(preserved.relative_path()).is_file());
}

#[test]
fn standalone_child_directory_and_terminal_commit_together_without_recovering_execution() {
    for (accepted, failure_table) in [
        (false, "body_appends"),
        (false, "child_tasks"),
        (true, "body_appends"),
        (true, "child_tasks"),
    ] {
        let home = home();
        let mut engine = open_engine(&home);
        let session = session_id("standalone-owner");
        let parent = run_id("standalone-parent");
        let child = child_task_id("standalone-child");
        seed_session_and_run(&mut engine, session.as_str(), parent.as_str());
        engine
            .create_child_task(NewStoredChildTask {
                context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                context_window_tokens: 8192,
                child_task_id: child.clone(),
                session_id: session.clone(),
                parent_run_id: parent.clone(),
                parent_tool_call_id: assistant_protocol::ToolCallId::new("standalone-call")
                    .unwrap(),
                title: "standalone directory".into(),
                system_prompt: SystemPromptSnapshot::new(vec!["private control prompt".into()]),
                agent_variant: assistant_protocol::AgentVariant::Build,
                created_at_ms: 2000,
            })
            .unwrap();
        if !accepted {
            engine
                .start_child_task(ChildTaskStart {
                    operation_id: "standalone-start".into(),
                    child_task_id: child.clone(),
                    session_id: session.clone(),
                    message: raw_user_message("standalone-user", "inspect"),
                    started_at_ms: 2100,
                })
                .unwrap();
        }
        engine.connection.execute_batch(&format!("CREATE TRIGGER pause_directory_commit BEFORE DELETE ON {failure_table} BEGIN SELECT RAISE(ABORT,'injected paired finalization failure'); END;")).unwrap();
        let status = if accepted {
            ChildTaskStatus::Cancelled
        } else {
            ChildTaskStatus::Completed
        };
        assert!(
            engine
                .settle_child_task(StoredChildTaskSettlement {
                    operation_id: "standalone-end".into(),
                    child_task_id: child.clone(),
                    session_id: session.clone(),
                    status,
                    cancel_requested: accepted,
                    error: None,
                    messages: if accepted {
                        Vec::new()
                    } else {
                        vec![assistant_message("standalone-final", "retained answer")]
                    },
                    final_message_id: if accepted {
                        None
                    } else {
                        Some(MessageId::new("standalone-final").unwrap())
                    },
                    finished_at_ms: 2200,
                })
                .is_err()
        );
        for table in ["child_tasks", "body_appends", "child_body_appends"] {
            assert_eq!(
                engine
                    .connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1,
                "{table}"
            );
        }
        let control = engine
            .load_child_task_control(&session, &child)
            .unwrap()
            .unwrap();
        assert_eq!(
            control.status,
            if accepted {
                ChildTaskStatus::Accepted
            } else {
                ChildTaskStatus::Running
            }
        );
        assert_eq!(control.message_count, if accepted { 0 } else { 1 });
        assert!(control.finished_at_ms.is_none());
        assert!(engine.load_conversation(&session).is_err());
        assert!(engine.load_child_conversation(&session, &child).is_err());
        let main_path = body_path(&engine.session_directory(&session).unwrap(), 1);
        let child_path = engine.child_body(&session, &child, 1).unwrap();
        let main_bytes = fs::read(&main_path).unwrap();
        let child_bytes = fs::read(&child_path).unwrap();
        engine
            .connection
            .execute_batch("DROP TRIGGER pause_directory_commit")
            .unwrap();
        // 两个文件已写完后，冲突尾部和不匹配的依赖都必须停止，不能只完成其中一份回执。
        let mut conflict = main_bytes.clone();
        conflict.extend_from_slice(b"conflicting tail");
        fs::write(&main_path, conflict).unwrap();
        assert!(
            engine
                .recover_body_appends()
                .unwrap()
                .contains(session.as_str())
        );
        assert!(
            engine
                .load_child_task_control(&session, &child)
                .unwrap()
                .is_some()
        );
        assert_eq!(fs::read(&child_path).unwrap(), child_bytes);
        fs::write(&main_path, &main_bytes).unwrap();
        let kind: String = engine
            .connection
            .query_row("SELECT kind FROM body_appends", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            engine
                .connection
                .execute(
                    "UPDATE body_appends SET kind=json_set(kind,'$.child.body_generation',99)",
                    []
                )
                .unwrap(),
            1
        );
        assert!(
            engine
                .recover_body_appends()
                .unwrap()
                .contains(session.as_str())
        );
        assert!(
            engine
                .load_child_task_control(&session, &child)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            engine
                .connection
                .execute("UPDATE body_appends SET kind='messages'", [])
                .unwrap(),
            1
        );
        assert!(
            engine
                .recover_child_storage()
                .unwrap()
                .contains(&(session.as_str().to_owned(), child.as_str().to_owned()))
        );
        assert!(
            engine
                .recover_body_appends()
                .unwrap()
                .contains(session.as_str())
        );
        assert!(
            engine
                .load_child_task_control(&session, &child)
                .unwrap()
                .is_some()
        );
        for table in ["body_appends", "child_body_appends"] {
            assert_eq!(
                engine
                    .connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        assert_eq!(
            engine
                .connection
                .execute("UPDATE body_appends SET kind=?1", [kind])
                .unwrap(),
            1
        );
        drop(engine);
        let mut engine = open_engine(&home);
        assert!(engine.recover_body_appends().unwrap().is_empty());
        assert!(engine.recover_child_storage().unwrap().is_empty());
        for table in [
            "child_tasks",
            "body_appends",
            "child_body_appends",
            "pending_tool_exchanges",
            "pending_tool_starts",
        ] {
            assert_eq!(
                engine
                    .connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{table}"
            );
        }
        assert_eq!(fs::read(&main_path).unwrap(), main_bytes);
        assert_eq!(fs::read(&child_path).unwrap(), child_bytes);
        assert!(
            engine
                .load_conversation(&session)
                .unwrap()
                .messages
                .is_empty()
        );
        assert_eq!(
            engine
                .load_child_conversation(&session, &child)
                .unwrap()
                .messages
                .len(),
            if accepted { 0 } else { 2 }
        );
        let history = engine.load_child_history_catalog(&session).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, Some(status));
        assert_eq!(history[0].finished_at_ms, Some(2200));
        assert_eq!(
            history[0].started_at_ms,
            if accepted { None } else { Some(2100) }
        );
        assert_eq!(history[0].context_window_tokens, Some(8192));
        assert_eq!(history[0], child_file_fact(&mut engine, &session, &child));
        assert_eq!(
            engine
                .inspect_session_deletion(&session)
                .unwrap()
                .child_task_count,
            1
        );
        assert!(engine.child_generation(&session, &child).is_err());
        assert!(
            engine
                .load_unfinished_child_tasks_scoped(Some(&session))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            engine
                .search_conversation_titles(ConversationSearchRequest {
                    query: "standalone directory".into(),
                    scope: ConversationSearchScope::Session {
                        session_id: session.clone()
                    },
                    limit: 1
                })
                .unwrap()
                .hits
                .len(),
            1
        );
        assert!(engine.recover_body_appends().unwrap().is_empty());
        assert!(engine.recover_child_storage().unwrap().is_empty());
        assert_eq!(fs::read(main_path).unwrap(), main_bytes);
        assert_eq!(fs::read(child_path).unwrap(), child_bytes);
    }
}

#[test]
fn interrupted_child_file_receipt_recovers_once_before_later_children_are_settled() {
    let home = home();
    let session = session_id("interrupted-child-owner");
    let parent = run_id("interrupted-parent");
    let mut engine = open_engine(&home);
    seed_session_and_run(&mut engine, session.as_str(), parent.as_str());
    for (index, name) in ["accepted-child", "running-child"].into_iter().enumerate() {
        let child = child_task_id(name);
        engine
            .create_child_task(NewStoredChildTask {
                context_management_mode: agent_model::ContextManagementMode::LocalSummary,
                context_window_tokens: 8192,
                child_task_id: child.clone(),
                session_id: session.clone(),
                parent_run_id: parent.clone(),
                parent_tool_call_id: assistant_protocol::ToolCallId::new(format!("call-{index}"))
                    .unwrap(),
                title: name.into(),
                system_prompt: SystemPromptSnapshot::new(vec!["child".into()]),
                agent_variant: assistant_protocol::AgentVariant::Build,
                created_at_ms: 2_000 + index as i64,
            })
            .unwrap();
        if index == 1 {
            engine
                .start_child_task(ChildTaskStart {
                    operation_id: "interrupt-child-start".into(),
                    child_task_id: child.clone(),
                    session_id: session.clone(),
                    message: raw_user_message("interrupt-child-user", "inspect"),
                    started_at_ms: 2_100,
                })
                .unwrap();
            engine
                .request_child_task_cancellation(&session, &child)
                .unwrap();
        }
    }
    engine.connection.execute_batch("CREATE TRIGGER pause_child_receipt BEFORE DELETE ON child_body_appends BEGIN SELECT RAISE(ABORT,'injected child commit failure'); END;").unwrap();
    assert!(
        engine
            .interrupt_nonterminal_child_tasks_scoped(Some(&session))
            .is_err()
    );
    let receipt: (i64, i64) = engine
        .connection
        .query_row(
            "SELECT message_count_delta,created_at_ms FROM child_body_appends",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(receipt.0, 0);
    assert_eq!(
        engine
            .load_child_task_control(&session, &child_task_id("accepted-child"))
            .unwrap()
            .unwrap()
            .status,
        ChildTaskStatus::Accepted
    );
    assert_eq!(
        engine
            .load_child_task_control(&session, &child_task_id("running-child"))
            .unwrap()
            .unwrap()
            .status,
        ChildTaskStatus::Running
    );
    assert!(
        engine
            .load_conversation_history(ConversationOwner::ChildTask {
                session_id: session.clone(),
                child_task_id: child_task_id("accepted-child"),
            })
            .is_err()
    );
    // 仅修正本测试故障钩子，保留已写帧和回执，实际重开再恢复。
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_child_receipt")
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    assert!(engine.recover_child_storage().unwrap().is_empty());
    engine
        .interrupt_nonterminal_child_tasks_scoped(Some(&session))
        .unwrap();
    let mut frozen = Vec::new();
    for (index, name) in ["accepted-child", "running-child"].into_iter().enumerate() {
        let child = child_task_id(name);
        let task = child_file_fact(&mut engine, &session, &child);
        assert_eq!(task.status, Some(ChildTaskStatus::Interrupted));
        assert_eq!(task.message_count, index as u64);
        assert_eq!(
            task.started_at_ms,
            if index == 0 { None } else { Some(2_100) }
        );
        assert_eq!(task.cancel_requested, index == 1);
        if index == 0 {
            assert_eq!(task.finished_at_ms, Some(receipt.1));
        }
        let path = engine.child_body(&session, &child, 1).unwrap();
        frozen.push((child, path.metadata().unwrap().len(), task.finished_at_ms));
    }
    assert!(
        engine
            .load_unfinished_child_tasks_scoped(Some(&session))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM child_body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    engine
        .interrupt_nonterminal_child_tasks_scoped(Some(&session))
        .unwrap();
    for (child, length, finished) in frozen {
        assert_eq!(
            engine
                .child_body(&session, &child, 1)
                .unwrap()
                .metadata()
                .unwrap()
                .len(),
            length
        );
        assert_eq!(
            child_file_fact(&mut engine, &session, &child).finished_at_ms,
            finished
        );
    }
}

#[test]
fn normal_store_rejects_legacy_bytes_without_recreating_sql_history() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("unexpected-legacy");
    let run = run_id("unexpected-legacy-run");
    seed_session_and_run(&mut engine, session.as_str(), run.as_str());
    let path = body_path(&engine.session_directory(&session).unwrap(), 1);
    let legacy = conversation::encode_messages(&[assistant_message("old", "legacy")]).unwrap();
    fs::write(&path, &legacy).unwrap();
    let owner = ConversationOwner::MainSession {
        session_id: session.clone(),
    };
    assert!(engine.load_conversation(&session).is_err());
    assert!(
        engine
            .load_conversation_window(ConversationWindowRequest {
                owner: owner.clone(),
                generation: 1,
                end: None,
                limit: 20,
            })
            .is_err()
    );
    assert!(engine.load_conversation_history(owner).is_err());
    assert!(
        engine
            .append_messages(AppendRequest {
                operation_id: "must-not-stage".into(),
                session_id: session,
                run_id: run,
                messages: vec![assistant_message("new", "must not append")],
                message_step: Some(1),
                created_at_ms: 1_100,
            })
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), legacy);
    assert_eq!(engine.connection.query_row(
        "SELECT (SELECT COUNT(*) FROM body_appends)+(SELECT COUNT(*) FROM sqlite_schema WHERE name='run_message_refs')",
        [], |row| row.get::<_, i64>(0),
    ).unwrap(), 0);
}

#[test]
fn message_feedback_is_file_only_and_its_runless_zero_message_receipt_recovers() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("feedback-owner");
    let run = run_id("feedback-run");
    seed_session_and_run(&mut engine, session.as_str(), run.as_str());
    engine
        .append_messages(AppendRequest {
            operation_id: "feedback-answer".into(),
            session_id: session.clone(),
            run_id: run.clone(),
            messages: vec![assistant_message("feedback-message", "answer")],
            message_step: Some(1),
            created_at_ms: 1_100,
        })
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='run_message_refs'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    super::finish_fixture_run(
        &mut engine,
        session.as_str(),
        run.as_str(),
        RunStatus::Completed,
        None,
        1200,
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id=?1",
                [run.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    engine.connection.execute_batch("CREATE TRIGGER pause_receipt BEFORE DELETE ON body_appends BEGIN SELECT RAISE(ABORT,'test interruption'); END;").unwrap();
    let message = assistant_protocol::MessageId::new("feedback-message").unwrap();
    assert!(
        engine
            .set_message_feedback(MessageFeedbackChange {
                session_id: session.clone(),
                message_id: message.clone(),
                feedback: Some(MessageFeedback::Positive),
                changed_at_ms: 2_000,
            })
            .is_err()
    );
    let receipt: (Option<String>, i64) = engine
        .connection
        .query_row(
            "SELECT run_id,message_count_delta FROM body_appends",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(receipt, (None, 0));
    assert!(engine.load_message_feedback(&session).is_err());
    assert!(engine.load_conversation(&session).is_err());
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
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_receipt")
        .unwrap();
    drop(engine);

    let mut engine = open_engine(&home);
    // 只收敛文件提交回执，不通过缺少 Input/Run 的测试夹具装配执行。
    assert!(engine.recover_body_appends().unwrap().is_empty());
    assert_eq!(
        engine.load_message_feedback(&session).unwrap()[0].feedback,
        MessageFeedback::Positive
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    engine
        .set_message_feedback(MessageFeedbackChange {
            session_id: session.clone(),
            message_id: message,
            feedback: None,
            changed_at_ms: 2_001,
        })
        .unwrap();
    assert!(engine.load_message_feedback(&session).unwrap().is_empty());
    let facts = engine
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: session.clone(),
        })
        .unwrap()
        .facts;
    assert_eq!(facts.feedback.len(), 1);
    assert_eq!(facts.feedback[0].feedback, None);
    assert_eq!(facts.feedback[0].changed_at_ms, 2_001);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT message_count FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        engine
            .set_message_feedback(MessageFeedbackChange {
                session_id: session,
                message_id: assistant_protocol::MessageId::new("missing-message").unwrap(),
                feedback: Some(MessageFeedback::Negative),
                changed_at_ms: 2_002,
            })
            .is_err()
    );
}

#[test]
fn zero_message_retry_start_and_settlement_survive_a_written_receipt_without_inventing_messages() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("zero-retry");
    let first = run_id("zero-first");
    let retry = run_id("zero-second");
    seed_session_and_run(&mut engine, session.as_str(), first.as_str());
    engine
        .append_messages(AppendRequest {
            operation_id: "zero-user".into(),
            session_id: session.clone(),
            run_id: first.clone(),
            messages: vec![user_message("user-zero-first", "already committed")],
            message_step: None,
            created_at_ms: 1_100,
        })
        .unwrap();
    engine
        .settle_run(StoredRunSettlement {
            operation_id: "zero-first-end".into(),
            session_id: session.clone(),
            run_id: first.clone(),
            status: RunStatus::Failed,
            cancel_requested: false,
            error: None,
            queued_user_message: None,
            messages: vec![],
            message_step: None,
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: 1_200,
        })
        .unwrap();
    // 可靠结算已经释放旧控制；重试必须直接核对文件。
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id=?1",
                [first.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    engine
        .create_run_attempt(assistant_runtime::NewStoredRunAttempt {
            run_id: retry.clone(),
            source_run_id: first.clone(),
            session_id: session.clone(),
            approval_mode: assistant_protocol::ApprovalMode::default(),
            created_at_ms: 2_000,
        })
        .unwrap();
    assert!(
        engine
            .create_run_attempt(assistant_runtime::NewStoredRunAttempt {
                run_id: run_id("zero-duplicate"),
                source_run_id: first,
                session_id: session.clone(),
                approval_mode: assistant_protocol::ApprovalMode::default(),
                created_at_ms: 2_001,
            })
            .is_err()
    );
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "zero-retry-start".into(),
            session_id: session.clone(),
            run_id: retry.clone(),
            input_id: InputId::new("input-zero-first").unwrap(),
            message: None,
            shell: None,
            reasoning_effort: None,
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            created_at_ms: 2_500,
        })
        .unwrap();
    engine
        .stage_append_for(
            AppendRequest {
                operation_id: "zero-retry-end".into(),
                session_id: session.clone(),
                run_id: retry.clone(),
                messages: vec![],
                message_step: None,
                created_at_ms: 2_600,
            },
            AppendPurpose::RunSettlement {
                status: RunStatus::Cancelled,
                cancel_requested: true,
                error: None,
                queued_user_message: None,
                goal_effect: None,
                proxy_report: None,
            },
        )
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row("SELECT message_count_delta FROM body_appends", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    engine.write_staged_append("zero-retry-end").unwrap();
    drop(engine);

    let mut engine = open_engine(&home);
    engine.load_runtime().unwrap();
    let owner = ConversationOwner::MainSession {
        session_id: session.clone(),
    };
    let path = body_path(&engine.session_directory(&session).unwrap(), 1);
    let document = conversation::read_document(&path, &owner, 1).unwrap();
    assert_eq!(document.conversation.messages.len(), 1);
    let history = document.history.unwrap();
    let fact = history
        .runs
        .iter()
        .find(|fact| fact.run_id == retry)
        .unwrap();
    assert_eq!(fact.status, Some(RunStatus::Cancelled));
    assert_eq!(fact.started_at_ms, Some(2_500));
    assert_eq!(fact.finished_at_ms, Some(2_600));
    assert_eq!(fact.attempt, 2);
    assert!(fact.cancel_requested);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT message_count FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn reliable_file_retains_run_step_and_time_without_sql_history_and_across_replacement() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("file-run");
    let run = run_id("file-run-one");
    seed_session_and_run(&mut engine, session.as_str(), run.as_str());
    let user = raw_user_message("user-file-run-one", "preserve my history");
    engine.connection.execute(
        "UPDATE inputs SET state='queued',priority_order=3,queued_message_json=?1 WHERE input_id='input-file-run-one'",
        [serde_json::to_string(&user).unwrap()],
    ).unwrap();
    let queued_history = engine
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: session.clone(),
        })
        .unwrap();
    assert!(queued_history.facts.inputs.is_empty() && queued_history.facts.runs.is_empty());
    let queued_payload: (String, i64) = engine.connection.query_row(
        "SELECT queued_message_json,priority_order FROM inputs WHERE input_id='input-file-run-one'",
        [], |row| Ok((row.get(0)?,row.get(1)?)),
    ).unwrap();
    assert_eq!(queued_payload, (serde_json::to_string(&user).unwrap(), 3));
    engine
        .commit_user_message(UserMessageCommit {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            operation_id: "file-start".into(),
            input_id: InputId::new("input-file-run-one").unwrap(),
            run_id: run.clone(),
            session_id: session.clone(),
            message: Some(user),
            reasoning_effort: None,
            created_at_ms: 2_000,
        })
        .unwrap();
    let consumed: (String, Option<String>, Option<i64>) = engine.connection.query_row(
        "SELECT state,queued_message_json,priority_order FROM inputs WHERE input_id='input-file-run-one'",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).unwrap();
    assert_eq!(consumed, ("committed".into(), None, None));
    engine
        .append_messages(AppendRequest {
            operation_id: "file-step".into(),
            session_id: session.clone(),
            run_id: run.clone(),
            messages: vec![assistant_message("first-answer", "first reliable step")],
            message_step: Some(1),
            created_at_ms: 2_050,
        })
        .unwrap();
    engine
        .settle_run(StoredRunSettlement {
            operation_id: "file-end".into(),
            run_id: run.clone(),
            session_id: session.clone(),
            status: RunStatus::Completed,
            cancel_requested: false,
            error: None,
            queued_user_message: None,
            messages: vec![assistant_message("final-answer", "second reliable step")],
            message_step: Some(2),
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: 2_100,
        })
        .unwrap();
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let owner = ConversationOwner::MainSession {
        session_id: session.clone(),
    };
    let path = body_path(&engine.session_directory(&session).unwrap(), 1);
    let document = conversation::read_document(&path, &owner, 1).unwrap();
    let expected = document.history.clone().unwrap();
    assert_eq!(expected.runs.len(), 1);
    assert_eq!(expected.runs[0].run_id, run);
    assert_eq!(expected.runs[0].status, Some(RunStatus::Completed));
    assert_eq!(expected.runs[0].started_at_ms, Some(2_000));
    assert_eq!(expected.runs[0].finished_at_ms, Some(2_100));
    let expected_impact = engine.inspect_session_deletion(&session).unwrap();
    assert_eq!(expected_impact.run_count, 1);
    assert_eq!(
        expected_impact.message_count,
        document.conversation.messages.len() as u64
    );
    assert_eq!(
        expected
            .message_origins
            .iter()
            .find(|origin| origin.message_id.as_str() == "final-answer")
            .unwrap()
            .step,
        Some(2)
    );

    // 事实只能由刚才的文件提交保存；提交完成即无 SQL 关联及终态 Run，之后仍完整回读。
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='run_message_refs'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id=?1",
                [run.as_str()],
                |row| row.get::<_, i64>(0)
            )
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
    assert_eq!(
        conversation::read_document(&path, &owner, 1)
            .unwrap()
            .history,
        Some(expected.clone())
    );
    assert_eq!(
        engine.inspect_session_deletion(&session).unwrap(),
        expected_impact
    );
    assert_eq!(
        engine
            .load_conversation_history(owner.clone())
            .unwrap()
            .facts,
        expected
    );

    let plan = engine
        .begin_replacement(session.clone(), document.conversation, 3_000)
        .unwrap();
    engine.commit_replacement(&plan).unwrap();
    let next = body_path(
        &engine.session_directory(&session).unwrap(),
        plan.new_generation,
    );
    assert_eq!(
        conversation::read_document(&next, &owner, plan.new_generation)
            .unwrap()
            .history,
        Some(expected)
    );
}

#[tokio::test]
async fn child_body_carries_its_binding_terminal_and_clock_without_a_terminal_control_row() {
    let home = home();
    let mut engine = open_engine(&home);
    let session = session_id("file-child");
    let parent = run_id("file-parent");
    let child = child_task_id("file-child-one");
    seed_session_and_run(&mut engine, session.as_str(), parent.as_str());
    begin_parent_exchange(
        &mut engine,
        &session,
        &parent,
        "call-1",
        "file-parent-receipt",
    );
    engine
        .create_child_task(NewStoredChildTask {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            context_window_tokens: 8192,
            child_task_id: child.clone(),
            session_id: session.clone(),
            parent_run_id: parent.clone(),
            parent_tool_call_id: assistant_protocol::ToolCallId::new("call-1").unwrap(),
            title: "inspect history".into(),
            system_prompt: SystemPromptSnapshot::new(vec!["child".into()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: 2_000,
        })
        .unwrap();
    engine
        .start_child_task(ChildTaskStart {
            operation_id: "file-child-start".into(),
            child_task_id: child.clone(),
            session_id: session.clone(),
            message: raw_user_message("child-user", "inspect"),
            started_at_ms: 2_100,
        })
        .unwrap();
    engine
        .request_child_task_cancellation(&session, &child)
        .unwrap();
    engine
        .settle_child_task(StoredChildTaskSettlement {
            operation_id: "file-child-end".into(),
            child_task_id: child.clone(),
            session_id: session.clone(),
            status: ChildTaskStatus::Completed,
            cancel_requested: false,
            error: None,
            messages: vec![assistant_message("child-answer", "done")],
            final_message_id: Some(MessageId::new("child-answer").unwrap()),
            finished_at_ms: 2_200,
        })
        .unwrap();
    let owner = ConversationOwner::ChildTask {
        session_id: session.clone(),
        child_task_id: child.clone(),
    };
    let path = engine.child_body(&session, &child, 1).unwrap();
    let before = conversation::read_document(&path, &owner, 1)
        .unwrap()
        .history
        .unwrap();
    assert_eq!(before.child_tasks.len(), 1);
    let fact = &before.child_tasks[0];
    assert_eq!(fact.parent_run_id, parent);
    assert_eq!(fact.parent_tool_call_id.as_str(), "call-1");
    assert_eq!(fact.status, Some(ChildTaskStatus::Completed));
    assert_eq!(
        (fact.started_at_ms, fact.finished_at_ms),
        (Some(2_100), Some(2_200))
    );
    assert_eq!(fact.message_count, 2);
    assert!(fact.cancel_requested);
    // 父工具交换可靠闭合后，其文件保存 child 的阅读定位；不另建 SQL 历史索引。
    complete_parent_exchange(
        &mut engine,
        &session,
        &parent,
        "call-1",
        "file-parent-receipt",
        &child,
        "done",
    )
    .unwrap();
    let expected_impact = engine.inspect_session_deletion(&session).unwrap();
    assert_eq!(expected_impact.run_count, 1);
    assert_eq!(expected_impact.child_task_count, 1);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM child_body_appends", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM child_tasks WHERE child_task_id=?1",
                [child.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM child_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        engine.inspect_session_deletion(&session).unwrap(),
        expected_impact
    );
    assert_eq!(
        conversation::read_document(&path, &owner, 1)
            .unwrap()
            .history,
        Some(before)
    );
    assert_eq!(
        engine
            .load_conversation_history(owner.clone())
            .unwrap()
            .facts
            .child_tasks[0]
            .status,
        Some(ChildTaskStatus::Completed)
    );
    assert_eq!(
        engine
            .load_child_conversation(&session, &child)
            .unwrap()
            .messages
            .len(),
        2
    );
    assert!(
        engine.child_generation(&session, &child).is_err(),
        "file history cannot become an execution target"
    );
    let search = ConversationSearchRequest {
        query: "done".into(),
        scope: ConversationSearchScope::Session {
            session_id: session.clone(),
        },
        limit: 1,
    };
    let page = engine.search_conversations(search.clone()).unwrap();
    assert_eq!(
        page.hits.len(),
        1,
        "terminal child is searchable from its file binding"
    );
    assert_eq!(page.hits[0].owner, owner);
    assert_eq!(
        page.hits[0].child_task_title.as_deref(),
        Some("inspect history")
    );
    engine.mark_recall_owner_dirty(&owner, 1, 2_400);
    drop(engine);

    let mut engine = open_engine(&home);
    let rebuilt = engine.search_conversations(search).unwrap();
    assert!(!rebuilt.partial);
    assert_eq!(
        rebuilt.hits.len(),
        1,
        "released child can rebuild its text index after reopening"
    );
    assert_eq!(rebuilt.hits[0].owner, owner);
    assert_eq!(
        rebuilt.hits[0].child_task_title.as_deref(),
        Some("inspect history")
    );
    let main_path = body_path(&engine.session_directory(&session).unwrap(), 1);
    drop(engine);

    let store = std::sync::Arc::new(LocalRuntimeStore::open(home.path(), 32).await.unwrap());
    let resources = crate::resources::HostResources::new(home.path()).unwrap();
    let runtime = assistant_runtime::AssistantRuntime::open_with_recall_key(
        assistant_runtime::RuntimeConfig::new(std::num::NonZeroUsize::new(32).unwrap()),
        std::sync::Arc::new(crate::config_source::LocalConfigSource::new(
            home.path().join("config.toml"),
        )),
        resources.model_factory,
        resources.session_environment_factory,
        resources.skill_package_source,
        resources.run_tool_factory,
        resources.child_task_workspace_factory,
        store.clone(),
        store.clone(),
        [31; 32],
    )
    .await
    .unwrap();
    assert!(
        store
            .load_session_state(&session)
            .await
            .unwrap()
            .state
            .child_tasks
            .is_empty()
    );
    let snapshot = runtime
        .get_child_task(assistant_protocol::GetChildTaskRequest {
            session_id: session.clone(),
            child_task_id: child.clone(),
        })
        .await
        .unwrap()
        .task;
    assert_eq!(snapshot.status, ChildTaskStatus::Completed);
    assert_eq!(snapshot.final_text, "done");
    assert_eq!(
        (snapshot.started_at_ms, snapshot.finished_at_ms),
        (Some(2_100), Some(2_200))
    );
    let detail = runtime
        .get_child_task_view(assistant_protocol::GetChildTaskViewRequest {
            session_id: session.clone(),
            child_task_id: child.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert!(!detail.task.can_cancel);
    assert_eq!(detail.task.task, snapshot);
    assert_eq!(detail.conversation.items.len(), 2);
    let cancelled = runtime
        .cancel_child_task(assistant_protocol::CancelChildTaskRequest {
            session_id: session.clone(),
            child_task_id: child.clone(),
        })
        .await
        .unwrap()
        .task;
    assert_eq!(cancelled, snapshot);
    assert!(
        runtime
            .get_child_task(assistant_protocol::GetChildTaskRequest {
                session_id: SessionId::new("other-owner").unwrap(),
                child_task_id: child,
            })
            .await
            .is_err()
    );
    assert!(
        store
            .load_session_state(&session)
            .await
            .unwrap()
            .state
            .child_tasks
            .is_empty()
    );
    let titles = runtime
        .search_conversation_history(assistant_protocol::SearchConversationHistoryRequest {
            session_id: session.clone(),
            query: "in".into(),
            scope: assistant_protocol::ConversationHistoryScope::Session,
            offset: 0,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(!titles.partial);
    assert!(
        titles
            .items
            .iter()
            .any(|hit| hit.child_task_title.as_deref() == Some("inspect history"))
    );
    // 只破坏本次隔离夹具；短查询和全文索引不可用时均保留可靠的 Session 标题及局部失败提示。
    fs::write(main_path, b"bad main history").unwrap();
    for query in ["Se", "Session"] {
        let page = runtime
            .search_conversation_history(assistant_protocol::SearchConversationHistoryRequest {
                session_id: session.clone(),
                query: query.into(),
                scope: assistant_protocol::ConversationHistoryScope::Session,
                offset: 0,
                limit: 10,
            })
            .await
            .unwrap();
        assert!(page.partial);
        assert!(
            page.failed_owners
                .contains(&ConversationOwner::MainSession {
                    session_id: session.clone()
                })
        );
        assert!(page.items.iter().any(|hit| hit.owner
            == ConversationOwner::MainSession {
                session_id: session.clone()
            }));
    }
    runtime
        .shutdown(assistant_protocol::ShutdownRuntimeRequest {})
        .await
        .unwrap();
}
