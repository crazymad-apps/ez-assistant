use super::*;

#[tokio::test]
async fn reload_and_start_race_keeps_database_credentials_independent() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let source = Arc::new(GatedConfigSource {
        document: "schema_version=1\n[runtime.model_transport]\nrequest_timeout_ms=10000".into(),
        entered: entered.clone(),
        release: release.clone(),
    });
    let old_model: Arc<dyn ModelService> = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        assistant_text("old-response", "old"),
    ));
    let new_model: Arc<dyn ModelService> = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        assistant_text("new-response", "new"),
    ));
    let factory = Arc::new(RecordingModelFactory::new([old_model, new_model]));
    let runtime = Arc::new(AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source,
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    ));
    runtime
        .config_registry
        .replace_document_for_test(TEST_CONFIG);
    model_fixture::seed(&runtime, "old-key").await;
    let before_reload = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("old session");
    let after_reload = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("new session");

    let old_run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: before_reload.session.session_id.clone(),
            message: "before swap".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("run from old snapshot");
    assert_eq!(
        wait_for_terminal(
            &runtime,
            &before_reload.session.session_id,
            &old_run.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Completed
    );

    let reload_runtime = runtime.clone();
    let reload = tokio::spawn(async move {
        reload_runtime
            .reload_config(ReloadConfigRequest::default())
            .await
    });
    entered.notified().await;

    let start_runtime = runtime.clone();
    let start_session_id = after_reload.session.session_id.clone();
    let new_run = tokio::spawn(async move {
        start_runtime
            .submit_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant: assistant_protocol::AgentVariant::Build,
                session_id: start_session_id,
                message: "race after swap".to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            })
            .await
    });

    release.notify_one();
    assert_eq!(
        reload
            .await
            .expect("reload task")
            .expect("reload result")
            .status
            .state,
        assistant_protocol::ConfigurationState::Ready
    );
    let new_run = new_run
        .await
        .expect("start task")
        .expect("run from new snapshot");
    assert_eq!(
        wait_for_terminal(
            &runtime,
            &after_reload.session.session_id,
            &new_run.run.run_id,
        )
        .await
        .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(factory.api_keys(), ["old-key", "old-key"]);
}

#[tokio::test]
async fn provider_changes_affect_future_runs_and_invalid_global_config_does_not_fall_back() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let mut registry = ToolRegistry::new();
    registry
        .register(
            ScriptedTool::hanging("slow_tool", OrderLog::new())
                .with_entered_signal(entered.clone())
                .with_cleanup_signal(cleanup.clone()),
        )
        .expect("register tool");

    let first_model: Arc<dyn ModelService> = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [ModelScript::Events(message_events(&assistant_tool_call(
            "assistant-tools",
            "slow_tool",
        )))],
    ));
    let second_model: Arc<dyn ModelService> = Arc::new(ScriptedModelService::completing(
        model_capabilities(true),
        8_192,
        assistant_text("assistant-final", "new credential run"),
    ));
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG.into()));
    let factory = Arc::new(RecordingModelFactory::new([first_model, second_model]));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source.clone(),
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(registry.snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    model_fixture::seed(&runtime, "old-key").await;
    assert_eq!(
        runtime
            .reload_config(ReloadConfigRequest::default())
            .await
            .expect("initial load")
            .status
            .state,
        assistant_protocol::ConfigurationState::Ready
    );
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first session");
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second session");
    set_auto_approval(&runtime, &first.session.session_id).await;
    set_auto_approval(&runtime, &second.session.session_id).await;

    let first_run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: first.session.session_id.clone(),
            message: "start with old credential".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("first run");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("first run remains active");

    runtime
        .update_provider(assistant_protocol::UpdateProviderRequest {
            provider_instance_id: test_model_selection("fixture").provider_instance_id,
            connection: model_fixture::provider("unused").connection,
            credential: assistant_protocol::ProviderCredentialChange::Replace(
                assistant_protocol::SecretValue::new("new-key".into()),
            ),
        })
        .await
        .unwrap();
    assert_eq!(
        runtime
            .reload_config(ReloadConfigRequest::default())
            .await
            .expect("reload")
            .status
            .state,
        assistant_protocol::ConfigurationState::Ready
    );
    let second_run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: second.session.session_id.clone(),
            message: "start with new credential".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("second run");
    assert_eq!(
        wait_for_terminal(&runtime, &second.session.session_id, &second_run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(factory.api_keys(), ["old-key", "new-key"]);

    // 全局配置损坏后不可接受新模型执行；既有活动 Run 仍可正常取消结算。
    source.replace(Some("invalid global TOML".into()));
    assert_eq!(
        runtime
            .reload_config(ReloadConfigRequest::default())
            .await
            .expect("invalid reload")
            .status
            .state,
        assistant_protocol::ConfigurationState::Invalid
    );
    let rejected_run = runtime
        .submit_input(test_input(
            &second.session.session_id,
            "must not use stale key",
        ))
        .await
        .unwrap()
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &second.session.session_id, &rejected_run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: second.session.session_id.clone(),
            })
            .await
            .expect("session after rejected input")
            .session
            .queued_input_count,
        1
    );
    assert_eq!(factory.api_keys(), ["old-key", "new-key"]);

    runtime
        .cancel_run(CancelRunRequest {
            session_id: first.session.session_id.clone(),
            run_id: first_run.run.run_id.clone(),
        })
        .await
        .expect("cancel first run");
    tokio::time::timeout(Duration::from_secs(1), cleanup.notified())
        .await
        .expect("first model cleanup");
    assert_eq!(
        wait_for_terminal(&runtime, &first.session.session_id, &first_run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
}

#[tokio::test]
async fn missing_and_unsafe_sources_are_normal_query_results() {
    let missing = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    assert_eq!(
        missing
            .get_config_status(GetConfigStatusRequest::default())
            .expect("initial status")
            .status
            .state,
        assistant_protocol::ConfigurationState::Missing
    );
    assert_eq!(
        missing
            .reload_config(ReloadConfigRequest::default())
            .await
            .expect("missing reload")
            .status
            .state,
        assistant_protocol::ConfigurationState::Missing
    );

    let unsafe_source = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        Arc::new(UnavailableConfigSource),
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    let result = unsafe_source
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("unsafe source is diagnostic result");
    assert_eq!(
        result.status.state,
        assistant_protocol::ConfigurationState::Invalid
    );
    assert_eq!(result.status.issues.len(), 1);
    assert_eq!(
        result.status.issues[0].code,
        assistant_protocol::ConfigurationIssueCode::UnsafeConfigSource
    );
}

#[tokio::test]
async fn provider_queries_are_redacted_and_invalid_updates_do_not_overwrite_storage() {
    let runtime = runtime(empty_model());
    let before = runtime.store.load_providers().await.unwrap();
    let mut invalid = before[0].connection.clone();
    invalid.endpoint = "https://api.example.test/v1?credential=unsafe".into();
    assert!(
        runtime
            .update_provider(assistant_protocol::UpdateProviderRequest {
                provider_instance_id: before[0].provider_instance_id.clone(),
                connection: invalid,
                credential: assistant_protocol::ProviderCredentialChange::Replace(
                    assistant_protocol::SecretValue::new("must-not-persist".into())
                ),
            })
            .await
            .is_err()
    );
    assert_eq!(runtime.store.load_providers().await.unwrap(), before);
    let projection = serde_json::to_string(&(
        runtime.list_providers().unwrap(),
        runtime.get_model_settings().unwrap(),
        runtime
            .get_config_status(GetConfigStatusRequest {})
            .unwrap(),
    ))
    .unwrap();
    assert!(!projection.contains("unique-test-secret-9f1ca2"));
    assert!(!projection.contains("must-not-persist"));
}

#[tokio::test]
async fn deleting_an_idle_session_provider_preserves_history_and_requires_reselection() {
    let runtime = runtime(empty_model());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session;
    runtime
        .set_session_model(SetSessionModelRequest {
            session_id: session.session_id.clone(),
            model_selection: Some(test_model_selection("fixture")),
        })
        .await
        .unwrap();
    let usage = runtime
        .delete_provider(test_model_selection("fixture").provider_instance_id)
        .await
        .unwrap();
    assert_eq!(usage.session_count, 1);
    assert_eq!(usage.fixed_config_count, 1);
    assert!(usage.default_model);
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session.session_id.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(
        view.session.model_selection,
        Some(test_model_selection("fixture"))
    );
    assert!(view.composer_capabilities.model_error.is_some());
    let failed = runtime
        .submit_input(test_input(&session.session_id, "cannot execute"))
        .await
        .unwrap()
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &session.session_id, &failed.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    assert!(
        runtime
            .conversation_snapshot(&session.session_id)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    let retained = runtime
        .get_session(GetSessionRequest {
            session_id: session.session_id.clone(),
        })
        .await
        .unwrap()
        .session;
    assert_eq!(retained.queued_input_count, 1);
    runtime
        .cancel_queued_input(assistant_protocol::CancelQueuedInputRequest {
            session_id: session.session_id.clone(),
            input_id: failed.input_id,
        })
        .await
        .unwrap();
    let replacement = runtime
        .create_provider(assistant_protocol::CreateProviderRequest {
            connection: model_fixture::provider("unused").connection,
            credential: assistant_protocol::ProviderCredentialChange::Replace(
                assistant_protocol::SecretValue::new("replacement-secret".into()),
            ),
        })
        .await
        .unwrap();
    let selection = assistant_protocol::ModelSelection {
        provider_instance_id: replacement.provider_instance_id,
        model_id: "fixture".into(),
    };
    runtime
        .save_model_fixed_config(assistant_protocol::SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: selection.clone(),
            parameters: model_fixture::parameters(),
        })
        .await
        .unwrap();
    runtime
        .set_session_model(SetSessionModelRequest {
            session_id: session.session_id.clone(),
            model_selection: Some(selection.clone()),
        })
        .await
        .unwrap();
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session_id
            })
            .await
            .unwrap()
            .session
            .model_selection,
        Some(selection)
    );
}

#[tokio::test]
async fn deleting_a_provider_does_not_interrupt_an_already_started_run() {
    let entered = Arc::new(Notify::new());
    let runtime = runtime(Arc::new(CancellationAwareModel {
        capabilities: model_capabilities(false),
        entered: entered.clone(),
    }));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session;
    let run = runtime
        .submit_input(test_input(&session.session_id, "running"))
        .await
        .unwrap()
        .run;
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    runtime
        .delete_provider(test_model_selection("fixture").provider_instance_id)
        .await
        .unwrap();
    let current = runtime
        .get_run(GetRunRequest {
            session_id: session.session_id.clone(),
            run_id: run.run_id.clone(),
        })
        .await
        .unwrap()
        .run;
    assert!(!current.status.is_terminal());
    assert!(!current.cancel_requested);
    runtime
        .interrupt_run(InterruptRunRequest {
            session_id: session.session_id.clone(),
            run_id: run.run_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session.session_id, &run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
}

fn test_input(session_id: &SessionId, message: &str) -> SubmitInputRequest {
    SubmitInputRequest {
        session_id: session_id.clone(),
        message: message.into(),
        mode: assistant_protocol::SubmitInputMode::Normal,
        variant: assistant_protocol::AgentVariant::Build,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    }
}
