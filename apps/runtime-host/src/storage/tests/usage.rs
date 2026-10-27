//! 只在新建隔离 Home 核对真实本地费用；继承／未知历史不能借换代或回填产生新费用。
use super::*;

#[test]
fn parent_and_child_statistics_are_transactional_snapshots_without_body_reads() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let root = tempfile::Builder::new()
        .prefix("usage-snapshot-")
        .tempdir_in(base)
        .unwrap();
    let mut engine = open_engine(&root);
    let session = session_id("statistics-only");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    // 旧结构单测宿主与正式 Host 都使用同一快照 SQL；没有任何生产数据。
    if !engine
        .connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='conversation_usage_snapshots')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    {
        engine
            .connection
            .execute_batch(crate::storage::usage::snapshot::SCHEMA)
            .unwrap();
    }
    let parent = ConversationOwner::MainSession {
        session_id: session.clone(),
    };
    let child = ConversationOwner::ChildTask {
        session_id: session.clone(),
        child_task_id: ChildTaskId::new("statistics-child").unwrap(),
    };
    for owner in [&parent, &child] {
        let response = assistant_message_with_usage("statistics-response", 100, Some(40));
        let transaction = engine.connection.transaction().unwrap();
        crate::storage::usage::record_usage_messages(&transaction, owner, [&response], 1).unwrap();
        transaction.rollback().unwrap();
        assert!(
            engine
                .get_conversation_usage(owner)
                .unwrap()
                .accumulated
                .is_none()
        );
        let transaction = engine.connection.transaction().unwrap();
        crate::storage::usage::record_usage_messages(&transaction, owner, [&response], 2).unwrap();
        transaction.commit().unwrap();
        let initial = engine.get_conversation_usage(owner).unwrap();
        assert_eq!(
            initial.completed_tokens,
            initial.accumulated.as_ref().map(|usage| usage.total_tokens)
        );
        let transaction = engine.connection.transaction().unwrap();
        crate::storage::usage::record_usage_messages(&transaction, owner, [&response], 2).unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            engine.get_conversation_usage(owner).unwrap(),
            initial,
            "replayed request must not double charge"
        );
        let input = user_message("statistics-next", "next request");
        let transaction = engine.connection.transaction().unwrap();
        crate::storage::usage::record_usage_messages(&transaction, owner, [&input], 3).unwrap();
        transaction.commit().unwrap();
        let advanced = engine.get_conversation_usage(owner).unwrap();
        assert_eq!(advanced.accumulated, initial.accumulated);
        assert!(advanced.used_tokens() > initial.used_tokens());
    }
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM conversation_usage_snapshots WHERE session_id=?1",
                [session.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM model_request_records WHERE session_id=?1",
                [session.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    // child 从未创建正文，查询快照也不能尝试打开文件。
    assert!(
        !child_task_directory(
            &engine.session_directory(&session).unwrap(),
            &ChildTaskId::new("statistics-child").unwrap()
        )
        .exists()
    );
}

fn summary(id: &str, input: u64) -> ConversationMessage {
    let mut message = context_summary(id, "local summary");
    let ConversationMessage::ContextSummary(summary) = &mut message else {
        unreachable!()
    };
    summary.usage = Some(TokenUsage {
        input_tokens: input,
        output_tokens: 3,
        total_tokens: input + 3,
        cached_input_tokens: None,
        reasoning_tokens: None,
    });
    message
}

fn request_count(engine: &StorageEngine, session: &SessionId) -> i64 {
    engine
        .connection
        .query_row(
            "SELECT COUNT(*) FROM model_request_records WHERE session_id=?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn fork_replacement_and_backfill_charge_only_the_new_local_summary() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let root = tempfile::Builder::new()
        .prefix("f02-local-usage-")
        .tempdir_in(base)
        .unwrap();
    let mut engine = open_engine(&root);
    let source = session_id("usage-source");
    seed_session_and_run(&mut engine, source.as_str(), "usage-source-run");
    engine
        .append_messages(AppendRequest {
            operation_id: "usage-source-turn".into(),
            session_id: source.clone(),
            run_id: run_id("usage-source-run"),
            messages: vec![
                user_message("user-usage-source-run", "request"),
                assistant_message_with_usage("inherited-turn", 100, Some(40)),
                summary("inherited-summary", 30),
            ],
            message_step: None,
            created_at_ms: 2000,
        })
        .unwrap();
    finish_fixture_run(
        &mut engine,
        source.as_str(),
        "usage-source-run",
        RunStatus::Completed,
        None,
        2100,
    );
    let source_usage = engine.get_session_usage(&source).unwrap();
    assert_eq!(source_usage.request_count, 2);
    let inherited = engine.load_conversation(&source).unwrap();

    for idle in [false, true] {
        let target = session_id(if idle {
            "usage-idle-fork"
        } else {
            "usage-replacement-fork"
        });
        engine
            .fork_session(SessionFork {
                source_session_id: source.clone(),
                source_generation: 1,
                session: new_session(target.as_str(), &engine.sessions_directory),
                conversation: inherited.clone(),
                attachments: Vec::new(),
                tool_images: Vec::new(),
                skill_activations: Vec::new(),
                mcp_selections: Vec::new(),
                session_commands: Vec::new(),
                work_plan: None,
                goal: None,
            })
            .unwrap();
        assert_eq!(request_count(&engine, &target), 0);
        let summary_id = format!("local-summary-{}", target);
        let mut messages = vec![summary(&summary_id, 7)];
        messages.extend(inherited.messages.clone());
        let replacement = ConversationSnapshot::new(messages);
        if idle {
            let operation_id = IdempotencyKey::new("usage-idle-compact").unwrap();
            engine
                .prepare_session_compaction(SessionHistoryCompactionPreparation {
                    operation_id: operation_id.clone(),
                    session_id: target.clone(),
                    expected_generation: 1,
                    created_at_ms: 3000,
                })
                .unwrap();
            engine
                .replace_context(ContextReplacement {
                    target: ContextReplacementTarget::IdleSession {
                        session_id: target.clone(),
                        expected_generation: 1,
                        operation_id,
                        compacted_message_count: 2,
                        retained_message_count: 1,
                    },
                    conversation: replacement,
                    changed_at_ms: 3100,
                })
                .unwrap();
        } else {
            let plan = engine
                .begin_replacement(target.clone(), replacement, 3100)
                .unwrap();
            engine.connection.execute_batch("CREATE TRIGGER fail_usage_switch BEFORE UPDATE OF body_generation ON sessions BEGIN SELECT RAISE(ABORT,'injected usage switch failure'); END;").unwrap();
            assert!(engine.commit_replacement(&plan).is_err());
            assert_eq!(request_count(&engine, &target), 0);
            assert_eq!(engine.session_generation(&target).unwrap(), 1);
            engine
                .connection
                .execute_batch("DROP TRIGGER fail_usage_switch")
                .unwrap();
            engine.commit_replacement(&plan).unwrap();
        }
        let expected = engine.get_session_usage(&target).unwrap();
        assert_eq!(request_count(&engine, &target), 1);
        assert_eq!(expected.request_count, 1);
        assert_eq!(expected.total_tokens, 10);
        assert_eq!(engine.get_session_usage(&source).unwrap(), source_usage);
        let facts = engine
            .load_conversation_history(ConversationOwner::MainSession {
                session_id: target.clone(),
            })
            .unwrap()
            .facts;
        let local = facts
            .message_origins
            .iter()
            .find(|origin| origin.message_id.as_str() == summary_id)
            .unwrap();
        assert!(!local.inherited);
        assert!(local.run_id.is_none() && local.step.is_none());

        // 模拟只需重建本地费用的隔离夹具；原继承正文仍有真实 usage，不得被回填为本地请求。
        engine
            .connection
            .execute(
                "DELETE FROM model_request_records WHERE session_id=?1",
                [target.as_str()],
            )
            .unwrap();
        engine.connection.execute("UPDATE session_usage SET request_count=0,input_tokens_sum=0,output_tokens_sum=0,total_tokens_sum=0,cached_input_tokens_sum=0,cached_request_count=0,reasoning_tokens_sum=0,reasoning_request_count=0,latest_input_tokens=NULL,latest_output_tokens=NULL,latest_total_tokens=NULL,latest_cached_input_tokens=NULL,latest_reasoning_tokens=NULL,backfilled=0 WHERE session_id=?1",[target.as_str()]).unwrap();
        engine.backfill_session_usage_scoped(Some(&target)).unwrap();
        assert_eq!(engine.get_session_usage(&target).unwrap(), expected);
        assert_eq!(request_count(&engine, &target), 1);
        engine.backfill_session_usage_scoped(Some(&target)).unwrap();
        assert_eq!(engine.get_session_usage(&target).unwrap(), expected);
    }
    drop(engine);
    let mut reopened = open_engine(&root);
    reopened.load_runtime().unwrap();
    for target in [
        session_id("usage-idle-fork"),
        session_id("usage-replacement-fork"),
    ] {
        assert_eq!(request_count(&reopened, &target), 1);
        assert_eq!(
            reopened.get_session_usage(&target).unwrap().total_tokens,
            10
        );
    }
    assert_eq!(reopened.get_session_usage(&source).unwrap(), source_usage);
}
