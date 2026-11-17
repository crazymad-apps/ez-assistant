//! 已有原生窗口经过分支、清空和失败重试后的公开入口回归。

use super::*;

async fn native_runtime(
    scripts: Vec<ModelScript>,
) -> (AssistantRuntime, Arc<NativeFactory>, SessionId) {
    let factory = Arc::new(NativeFactory {
        script: Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8192,
            scripts,
        )),
        compactions: Arc::new(AtomicUsize::new(0)),
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        64,
    );
    let mut provider = model_fixture::provider("synthetic");
    provider.connection.provider_type = assistant_protocol::ProviderType::Openai;
    provider.connection.protocol_preference =
        assistant_protocol::ProviderProtocolPreference::Responses;
    provider.server_history_key = Some("native-rebuild-provider".into());
    runtime.store.put_provider(provider).await.unwrap();
    let mut parameters = model_fixture::parameters();
    parameters.server_compaction_enabled = true;
    parameters.server_compaction_support = assistant_protocol::ModelFeatureSupport::Supported;
    model_fixture::save_fixed(&runtime, "fixture", parameters).await;
    runtime.restore_model_settings().await.unwrap();
    let id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    (runtime, factory, id)
}

fn answer(id: &str) -> ModelScript {
    ModelScript::Events(message_events(&assistant_text_with_usage(id, id, 120)))
}

async fn compact(runtime: &AssistantRuntime, id: &SessionId, key: &str) {
    let (generation, _) = edit_basis(runtime, id).await;
    let result = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new(key).unwrap(),
            expected_generation: generation,
        })
        .await
        .unwrap_or_else(|error| panic!("compaction {key} failed: {error:?}"));
    assert!(matches!(
        result.outcome,
        assistant_protocol::CompactSessionOutcome::Compacted { .. }
    ));
    let history = runtime.store.load_conversation(id).await.unwrap();
    let session = runtime.session(id).await.unwrap();
    assert_eq!(
        session
            .lock_state()
            .unwrap()
            .journal
            .as_ref()
            .unwrap()
            .snapshot(),
        history.project_provider_context().unwrap()
    );
}

#[tokio::test]
async fn native_fork_uses_checkpoint_at_its_cut_and_clear_discards_it() {
    let (runtime, factory, source) = native_runtime(vec![
        answer("before-checkpoint"),
        answer("after-checkpoint"),
        answer("early-fork-answer"),
        answer("late-fork-answer"),
        answer("after-clear"),
    ])
    .await;
    submit_completed_turn(&runtime, &source, "before checkpoint").await;
    compact(&runtime, &source, "source-compact").await;
    submit_completed_turn(&runtime, &source, "after checkpoint").await;
    let source_history = runtime.store.load_conversation(&source).await.unwrap();
    let (generation, _) = edit_basis(&runtime, &source).await;
    for (point, has_checkpoint) in [("before-checkpoint", false), ("after-checkpoint", true)] {
        let fork = runtime
            .fork_session(assistant_protocol::ForkSessionRequest {
                session_id: source.clone(),
                fork_point: assistant_protocol::MessageId::new(point).unwrap(),
                expected_generation: generation,
            })
            .await
            .unwrap()
            .session
            .session_id;
        factory.requests.lock().unwrap().clear();
        submit_completed_turn(&runtime, &fork, "continue fork").await;
        {
            let requests = factory.requests.lock().unwrap();
            let messages = &requests.last().unwrap().conversation.messages;
            assert_eq!(
                messages.iter().any(|message| matches!(message,
                    ConversationMessage::Assistant(assistant) if assistant.id.as_str() == "native-0")),
                has_checkpoint
            );
            assert_eq!(
                messages.iter().any(|message| matches!(message,
                    ConversationMessage::Assistant(assistant) if assistant.id.as_str() == "before-checkpoint")),
                !has_checkpoint
            );
        }
        // 分支不仅能发请求，还须再次通过持久历史与 Journal 的压缩一致性检查。
        compact(&runtime, &fork, &format!("fork-compact-{point}")).await;
        if has_checkpoint {
            let (generation, _) = edit_basis(&runtime, &fork).await;
            runtime
                .clear_session(assistant_protocol::ClearSessionRequest {
                    session_id: fork.clone(),
                    operation_id: assistant_protocol::IdempotencyKey::new("clear-fork").unwrap(),
                    expected_generation: generation,
                })
                .await
                .unwrap();
            submit_completed_turn(&runtime, &fork, "fresh after clear").await;
            let requests = factory.requests.lock().unwrap();
            assert!(!requests.last().unwrap().conversation.messages.iter().any(
                |message| matches!(message, ConversationMessage::Assistant(assistant)
                    if assistant.purpose == AssistantMessagePurpose::ContextCompaction)
            ));
        }
    }
    assert_eq!(
        runtime.store.load_conversation(&source).await.unwrap(),
        source_history
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn native_retry_after_runtime_restart_preserves_checkpoint_and_compacts_again() {
    let (runtime, factory, id) = native_runtime(vec![
        answer("initial-answer"),
        ModelScript::FailEstablishment(ModelError::Protocol("synthetic failure".into())),
        answer("retry-answer"),
    ])
    .await;
    submit_completed_turn(&runtime, &id, "initial input").await;
    compact(&runtime, &id, "before-failure").await;
    let failed = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            session_id: id.clone(),
            message: "input before failure".into(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &id, &failed.run.run_id)
            .await
            .status,
        RunStatus::Failed
    );
    let before = runtime.store.load_conversation(&id).await.unwrap();
    let store = runtime.store.clone();
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
    // 新 Runtime 仅复用隔离内存 Store，不能以清空 journal 模拟完整的失败恢复。
    let reopened = AssistantRuntime::open(
        RuntimeConfig::new(NonZeroUsize::new(64).unwrap()),
        Arc::new(MissingConfigSource),
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
        store,
        Arc::new(crate::permission::VolatilePermissionFileStore::default()),
    )
    .await
    .unwrap();
    reopened
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    reopened.restore_model_settings().await.unwrap();
    factory.requests.lock().unwrap().clear();
    let retry = reopened
        .retry_run(assistant_protocol::RetryRunRequest {
            session_id: id.clone(),
            run_id: failed.run.run_id,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&reopened, &id, &retry.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    {
        let requests = factory.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].conversation,
            before.project_provider_context().unwrap()
        );
    }
    compact(&reopened, &id, "after-retry").await;
    let after = reopened.store.load_conversation(&id).await.unwrap();
    assert_eq!(&after.messages[..before.messages.len()], &before.messages);
    assert_eq!(factory.compactions.load(Ordering::SeqCst), 2);
    reopened
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}
