//! 启动只恢复可查询状态，局部恢复失败不能拖垮其他会话或触发执行。

use super::store::FaultInjectingStore;
use super::*;

#[tokio::test]
async fn skill_state_preparation_failure_atomically_commits_the_failed_input() {
    let store = Arc::new(FaultInjectingStore::healthy());
    let runtime = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    runtime
        .session_for_test(&session.session_id)
        .await
        .lock_state()
        .expect("state")
        .queue_paused_by_user = true;
    let accepted = runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session_id.clone(),
            message: "skill state must not lose this input".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("accepted input");
    store.fail_next_skill_state_load();
    runtime
        .resume_session(assistant_protocol::ResumeSessionRequest {
            session_id: session.session_id.clone(),
        })
        .await
        .expect("resume queue");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session_id, &accepted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    let conversation = runtime
        .conversation_snapshot(&session.session_id)
        .await
        .expect("failed input conversation");
    assert_eq!(conversation.messages.len(), 1);
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session_id,
            })
            .await
            .expect("session")
            .session
            .queued_input_count,
        0
    );
}

#[tokio::test]
async fn session_view_repairs_only_its_terminal_queued_input_once() {
    let store = Arc::new(FaultInjectingStore::healthy());
    let first = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    let session = first
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    first
        .session_for_test(&session.session_id)
        .await
        .lock_state()
        .expect("state")
        .queue_paused_by_user = true;
    let source = first
        .submit_input(SubmitInputRequest {
            session_id: session.session_id.clone(),
            message: "recover only this input".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("source input");
    let unrelated = first
        .submit_input(SubmitInputRequest {
            session_id: session.session_id.clone(),
            message: "stay queued".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("unrelated input");
    drop(first);
    store.force_terminal_run_with_queued_input(
        &source.run.run_id,
        assistant_protocol::RunStatus::Failed,
        3_000,
    );

    let restarted = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    for _ in 0..2 {
        let view = restarted
            .get_session_view(assistant_protocol::GetSessionViewRequest {
                session_id: session.session_id.clone(),
            })
            .await
            .expect("prepared session view")
            .snapshot
            .value;
        assert_eq!(view.queue.items.len(), 1);
    }
    let persisted = store.load_session_state(&session.session_id).await.unwrap();
    let source_input = persisted
        .state
        .inputs
        .iter()
        .find(|input| input.input_id == source.input_id)
        .expect("source input");
    assert_eq!(source_input.state, crate::StoredInputState::Committed);
    assert!(source_input.queued_message.is_none());
    let unrelated_input = persisted
        .state
        .inputs
        .iter()
        .find(|input| input.input_id == unrelated.input_id)
        .expect("unrelated input");
    assert_eq!(unrelated_input.state, crate::StoredInputState::Queued);
    assert!(unrelated_input.queued_message.is_some());
    let conversation = store
        .load_conversation(&session.session_id)
        .await
        .expect("reconciled conversation");
    assert_eq!(conversation.messages.len(), 1);
    assert!(matches!(
        &conversation.messages[0],
        ConversationMessage::User(message)
            if message.parts.iter().any(|part| matches!(
                part,
                UserPart::Text(text) if text.text == "recover only this input"
            ))
    ));
}

#[tokio::test]
async fn unavailable_session_skips_settlement_and_cannot_resume_after_restart() {
    // 此 Store 拒绝全部结算；若启动仍尝试结算隔离会话，open 会直接失败。
    let store = Arc::new(FaultInjectingStore::fail_settlement());
    let first = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    let broken = first
        .create_session(Default::default())
        .await
        .expect("broken session")
        .session
        .session_id;
    let healthy = first
        .create_session(Default::default())
        .await
        .expect("healthy session")
        .session
        .session_id;
    let session = first.session_for_test(&broken).await;
    session.lock_state().expect("state").queue_paused_by_user = true;
    let accepted = first
        .submit_input(SubmitInputRequest {
            session_id: broken.clone(),
            message: "persisted before restart".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: vec![],
            quotes: vec![],
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("queued input");
    let message = session
        .lock_state()
        .expect("state")
        .inputs
        .get(&accepted.input_id)
        .expect("input")
        .stored
        .queued_message
        .clone()
        .expect("message");
    store
        .commit_user_message(crate::UserMessageCommit {
            shell: None,
            operation_id: "startup-fixture".to_owned(),
            input_id: accepted.input_id,
            run_id: accepted.run.run_id.clone(),
            session_id: broken.clone(),
            message: Some(message),
            reasoning_effort: None,
            created_at_ms: 100,
        })
        .await
        .expect("persist running");
    store.isolate_on_recovery(broken.clone());
    drop(first);
    let restarted = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    assert!(
        restarted
            .get_session(GetSessionRequest {
                session_id: healthy
            })
            .await
            .is_ok()
    );
    assert_eq!(
        restarted
            .session_for_test(&broken)
            .await
            .lock_state()
            .expect("isolated state")
            .runs
            .get(&accepted.run.run_id)
            .expect("run retained")
            .snapshot()
            .status,
        assistant_protocol::RunStatus::Interrupted,
    );
    assert_eq!(
        restarted
            .get_run(assistant_protocol::GetRunRequest {
                session_id: broken.clone(),
                run_id: accepted.run.run_id.clone(),
            })
            .await
            .expect("isolated run metadata remains queryable")
            .run
            .status,
        assistant_protocol::RunStatus::Interrupted,
    );
    assert!(matches!(
        restarted
            .resume_session(assistant_protocol::ResumeSessionRequest { session_id: broken })
            .await,
        Err(RuntimeError::SessionFaulted { .. })
    ));
    let persisted = store.load_runtime().await.expect("store still readable");
    assert_eq!(
        persisted
            .runs
            .iter()
            .find(|run| run.run_id == accepted.run.run_id)
            .expect("original run retained")
            .status,
        assistant_protocol::RunStatus::Running
    );
}

#[tokio::test]
async fn pending_title_does_not_schedule_history_reads_on_restart() {
    let store = Arc::new(FaultInjectingStore::fail_settlement());
    let first = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    first
        .create_session(Default::default())
        .await
        .expect("session");
    drop(first);
    store.pending_title_on_recovery();
    let before = store.conversation_load_count();
    let restarted = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    // shutdown 等待所有已注册后台任务；不能只在 spawn 尚未调度时断言零读取。
    restarted
        .shutdown(Default::default())
        .await
        .expect("shutdown");
    assert_eq!(store.conversation_load_count(), before);
}

#[tokio::test]
async fn startup_and_summary_pages_do_not_hydrate_history_and_first_load_is_single_flight() {
    let store = Arc::new(FaultInjectingStore::fail_settlement());
    let first = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).unwrap()),
    )
    .await;
    let mut ids = Vec::new();
    for _ in 0..105 {
        ids.push(
            first
                .create_session(Default::default())
                .await
                .unwrap()
                .session
                .session_id,
        );
    }
    drop(first);
    let restarted = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).unwrap()),
    )
    .await;
    assert!(restarted.sessions.read().unwrap().is_empty());
    let snapshot = restarted
        .get_application_snapshot(Default::default())
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(snapshot.active_sessions.len(), 100);
    assert_eq!(snapshot.active_sessions_next_offset, Some(100));
    let tail = restarted
        .list_sessions(assistant_protocol::ListSessionsRequest {
            offset: 100,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(tail.sessions.len(), 5);
    assert!(!tail.has_more);
    assert_eq!(store.full_load_count(), 0);
    assert!(store.loaded_session_ids().is_empty());
    assert_eq!(store.conversation_load_count(), 0);
    let (left, right) = tokio::join!(restarted.session(&ids[7]), restarted.session(&ids[7]));
    let (left, right) = (left.unwrap(), right.unwrap());
    assert!(Arc::ptr_eq(&left, &right));
    assert_eq!(store.loaded_session_ids(), vec![ids[7].clone()]);
    assert_eq!(restarted.sessions.read().unwrap().len(), 1);
    assert!(!left.lock_state().unwrap().execution_prepared);
    assert_eq!(store.conversation_load_count(), 0);
    // 超过软上限后只释放无人引用的历史缓存，仍在使用的 Arc 保持原身份。
    for id in &ids[..40] {
        restarted.session(id).await.unwrap();
    }
    assert!(restarted.sessions.read().unwrap().len() <= 32);
    assert!(Arc::ptr_eq(
        &left,
        &restarted.session(&ids[7]).await.unwrap()
    ));
    restarted.shutdown(Default::default()).await.unwrap();
    assert_eq!(store.full_load_count(), 0);
    assert_eq!(store.conversation_load_count(), 0);
}
