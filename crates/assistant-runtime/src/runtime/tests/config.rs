use super::*;

#[tokio::test]
async fn reload_and_start_race_observes_one_complete_configuration_snapshot() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let source = Arc::new(GatedConfigSource {
        document: config_with_api_key("new-key"),
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
        ToolSetSnapshot::default(),
        Arc::new(AllowAllAuthorizer),
    ));
    runtime
        .config_registry
        .replace_document_for_test(&config_with_api_key("old-key"));
    let before_reload = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("old session");
    let after_reload = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("new session");

    let reload_runtime = runtime.clone();
    let reload = tokio::spawn(async move {
        reload_runtime
            .reload_config(ReloadConfigRequest::default())
            .await
    });
    entered.notified().await;

    let old_run = runtime
        .submit_input(SubmitInputRequest {
            session_id: before_reload.session.session_id.clone(),
            message: "race before swap".to_owned(),
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
    let new_run = runtime
        .submit_input(SubmitInputRequest {
            session_id: after_reload.session.session_id.clone(),
            message: "run after swap".to_owned(),
            idempotency_key: None,
        })
        .await
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
    assert_eq!(factory.api_keys(), ["old-key", "new-key"]);
}

#[tokio::test]
async fn reload_changes_only_future_run_compilation_and_never_falls_back() {
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
    let source = Arc::new(MutableConfigSource::new(config_with_api_key("old-key")));
    let factory = Arc::new(RecordingModelFactory::new([first_model, second_model]));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source.clone(),
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        registry.snapshot(),
        Arc::new(AllowAllAuthorizer),
    );
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

    let first_run = runtime
        .submit_input(SubmitInputRequest {
            session_id: first.session.session_id.clone(),
            message: "start with old credential".to_owned(),
            idempotency_key: None,
        })
        .await
        .expect("first run");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("first run remains active");

    source.replace(Some(config_with_api_key("new-key")));
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
            session_id: second.session.session_id.clone(),
            message: "start with new credential".to_owned(),
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

    // 配置消失后立刻 fail-closed；既有活动 Run 仍可按原 cancellation 路径结算。
    source.replace(None);
    assert_eq!(
        runtime
            .reload_config(ReloadConfigRequest::default())
            .await
            .expect("missing reload")
            .status
            .state,
        assistant_protocol::ConfigurationState::Missing
    );
    let rejected = runtime
        .submit_input(SubmitInputRequest {
            session_id: second.session.session_id.clone(),
            message: "must not use stale key".to_owned(),
            idempotency_key: None,
        })
        .await
        .expect("input acceptance does not require an active model configuration");
    assert_eq!(
        wait_for_terminal(&runtime, &second.session.session_id, &rejected.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
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
async fn configuration_queries_are_complete_and_never_project_secrets() {
    let document = format!(
        r#"{TEST_CONFIG}

[models.invalid]
protocol = "chat_completions"
provider = "fixture"
endpoint = "https://api.example.test/v1"
model = "invalid-model"
context_window_tokens = 8192
max_output_tokens = 4096
"#
    );
    let source = Arc::new(MutableConfigSource::new(document));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source.clone(),
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        Arc::new(AllowAllAuthorizer),
    );
    let reloaded = runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("reload");
    assert_eq!(
        reloaded.status.state,
        assistant_protocol::ConfigurationState::Degraded
    );
    assert_eq!(
        reloaded.status.config_path.as_deref(),
        Some("/private/runtime/config.toml")
    );

    let models = runtime
        .list_models(ListModelsRequest::default())
        .expect("models");
    assert_eq!(models.models.len(), 2);
    let invalid_key = assistant_protocol::ModelKey::new("invalid").expect("model key");
    let invalid = runtime
        .get_model(GetModelRequest {
            model_key: invalid_key.clone(),
        })
        .expect("invalid model remains queryable")
        .model;
    assert!(!invalid.is_valid);
    assert!(invalid.issues.iter().any(|issue| {
        issue.code == assistant_protocol::ConfigurationIssueCode::MissingCredential
            && issue.model_key.as_ref() == Some(&invalid_key)
    }));
    let serialized = serde_json::to_string(&(reloaded, models, invalid)).expect("serialize");
    assert!(!serialized.contains("unique-test-secret-9f1ca2"));
    assert!(!serialized.contains("api_key\":"));

    let missing = assistant_protocol::ModelKey::new("missing").expect("model key");
    assert!(matches!(
        runtime.get_model(GetModelRequest {
            model_key: missing.clone(),
        }),
        Err(RuntimeError::ModelNotFound { model_key }) if model_key == missing
    ));

    source.replace(Some(
        "schema_version = 1\ndefault_model = \"fixture\"\napi_key = \"unique-test-secret-9f1ca2\"\n[".to_owned(),
    ));
    let invalid_reload = runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("invalid config is a diagnostic result");
    assert_eq!(
        invalid_reload.status.state,
        assistant_protocol::ConfigurationState::Invalid
    );
    assert!(
        invalid_reload.status.issues.iter().any(|issue| {
            issue.code == assistant_protocol::ConfigurationIssueCode::InvalidSyntax
        })
    );
    assert!(
        runtime
            .list_models(ListModelsRequest::default())
            .expect("invalid list")
            .models
            .is_empty()
    );
    assert!(
        !serde_json::to_string(&invalid_reload)
            .expect("serialize invalid reload")
            .contains("unique-test-secret-9f1ca2")
    );
}

#[tokio::test]
async fn missing_and_unsafe_sources_are_normal_query_results() {
    let missing = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        Arc::new(AllowAllAuthorizer),
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
        ToolSetSnapshot::default(),
        Arc::new(AllowAllAuthorizer),
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
