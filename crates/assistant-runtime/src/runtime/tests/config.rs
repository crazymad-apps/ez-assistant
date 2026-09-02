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
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
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

    let old_run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: before_reload.session.session_id.clone(),
            message: "before swap".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
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
        static_run_tool_factory(registry.snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
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
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: second.session.session_id.clone(),
            message: "start with new credential".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
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
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant: assistant_protocol::AgentVariant::Build,
                session_id: second.session.session_id.clone(),
                message: "must not use stale key".to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::ModelUnavailable { .. })
    ));
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: second.session.session_id.clone(),
            })
            .expect("session after rejected input")
            .session
            .queued_input_count,
        0
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
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
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
async fn model_mutations_use_revision_cas_and_never_publish_invalid_candidates() {
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG.to_owned()));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source.clone(),
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    let initial = runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("initial reload");
    let initial_revision = initial.status.revision.expect("initial revision");

    let created = runtime
        .create_model(CreateModelRequest {
            model: model_input(
                "secondary",
                "https://api.example.test/v1",
                assistant_protocol::ModelCredentialChange::Replace(
                    assistant_protocol::SecretValue::new("secondary-secret".to_owned()),
                ),
            ),
            expected_revision: Some(initial_revision),
            set_default: false,
        })
        .await
        .expect("create model");
    let created_revision = created.status.revision.clone().expect("created revision");
    assert!(created.models.iter().any(|model| {
        model
            .model_key
            .as_ref()
            .is_some_and(|key| key.as_str() == "secondary")
            && model.is_valid
    }));
    let serialized = serde_json::to_string(&created).expect("serialize mutation result");
    assert!(!serialized.contains("secondary-secret"));

    let persisted_before_invalid = source.document.lock().expect("source lock").clone();
    let invalid = runtime
        .create_model(CreateModelRequest {
            model: model_input(
                "invalid",
                "https://api.example.test/v1?credential=unsafe",
                assistant_protocol::ModelCredentialChange::Replace(
                    assistant_protocol::SecretValue::new("must-not-persist".to_owned()),
                ),
            ),
            expected_revision: Some(created_revision.clone()),
            set_default: false,
        })
        .await;
    assert!(matches!(invalid, Err(RuntimeError::InvalidRequest { .. })));
    assert_eq!(
        *source.document.lock().expect("source lock"),
        persisted_before_invalid
    );

    let mut external = persisted_before_invalid.expect("persisted document");
    external.push_str("\n# external edit\n");
    source.replace(Some(external.clone()));
    let secondary = assistant_protocol::ModelKey::new("secondary").expect("secondary key");
    let conflict = runtime
        .set_default_model(SetDefaultModelRequest {
            model_key: secondary,
            expected_revision: created_revision,
        })
        .await;
    assert!(matches!(conflict, Err(RuntimeError::ConfigurationConflict)));
    assert_eq!(
        runtime
            .get_config_status(GetConfigStatusRequest::default())
            .expect("status after conflict")
            .status
            .revision,
        Some(test_config_revision(&external))
    );
}

#[tokio::test]
async fn deleting_an_idle_session_model_preserves_history_and_requires_reselection() {
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG.to_owned()));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source,
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    let loaded = runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("reload");
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let created = runtime
        .create_model(CreateModelRequest {
            model: model_input(
                "secondary",
                "https://api.example.test/v1",
                assistant_protocol::ModelCredentialChange::Replace(
                    assistant_protocol::SecretValue::new("secondary-secret".to_owned()),
                ),
            ),
            expected_revision: loaded.status.revision,
            set_default: false,
        })
        .await
        .expect("create replacement");

    runtime
        .delete_model(DeleteModelRequest {
            model_key: assistant_protocol::ModelKey::new("fixture").expect("fixture key"),
            expected_revision: created.status.revision.expect("created revision"),
            replacement_default: Some(
                assistant_protocol::ModelKey::new("secondary").expect("secondary key"),
            ),
        })
        .await
        .expect("idle session does not block deletion");

    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session.session.session_id.clone(),
        })
        .await
        .expect("history remains readable")
        .snapshot
        .value;
    assert_eq!(view.session.model_key.as_str(), "fixture");
    assert!(view.composer_capabilities.selected_model_key.is_none());
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session.session.session_id.clone(),
                message: "must not be accepted".to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::ModelUnavailable { .. })
    ));
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone(),
            })
            .expect("session after rejected input")
            .session
            .queued_input_count,
        0
    );

    runtime
        .set_session_model(SetSessionModelRequest {
            session_id: session.session.session_id.clone(),
            model_key: assistant_protocol::ModelKey::new("secondary").expect("secondary key"),
        })
        .await
        .expect("select replacement model");
    let selected = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session.session.session_id,
        })
        .await
        .expect("selected session view")
        .snapshot
        .value
        .composer_capabilities
        .selected_model_key;
    assert_eq!(
        selected.as_ref().map(assistant_protocol::ModelKey::as_str),
        Some("secondary")
    );
}

#[tokio::test]
async fn deleting_a_model_used_by_a_running_run_is_rejected() {
    let entered = Arc::new(Notify::new());
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG.to_owned()));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source,
        Arc::new(StaticModelFactory::new(Arc::new(CancellationAwareModel {
            capabilities: model_capabilities(false),
            entered: entered.clone(),
        }))),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    let loaded = runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("reload");
    let created = runtime
        .create_model(CreateModelRequest {
            model: model_input(
                "secondary",
                "https://api.example.test/v1",
                assistant_protocol::ModelCredentialChange::Replace(
                    assistant_protocol::SecretValue::new("secondary-secret".to_owned()),
                ),
            ),
            expected_revision: loaded.status.revision,
            set_default: false,
        })
        .await
        .expect("create replacement");
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "running".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("run");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("model entered");

    assert!(matches!(
        runtime
            .delete_model(DeleteModelRequest {
                model_key: assistant_protocol::ModelKey::new("fixture").expect("fixture key"),
                expected_revision: created.status.revision.expect("created revision"),
                replacement_default: Some(
                    assistant_protocol::ModelKey::new("secondary").expect("secondary key"),
                ),
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));

    runtime
        .interrupt_run(InterruptRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: run.run.run_id.clone(),
        })
        .await
        .expect("interrupt");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
}

#[tokio::test]
async fn auxiliary_vision_model_mutation_is_capability_checked_and_clearable() {
    let document = format!("{TEST_CONFIG}\n[models.fixture.capabilities]\nimage_input = true\n");
    let source = Arc::new(MutableConfigSource::new(document));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source.clone(),
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    let loaded = runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("reload");
    let selected = runtime
        .set_auxiliary_vision_model(assistant_protocol::SetAuxiliaryVisionModelRequest {
            model_key: Some(assistant_protocol::ModelKey::new("fixture").expect("key")),
            expected_revision: loaded.status.revision.expect("revision"),
        })
        .await
        .expect("select auxiliary vision model");
    assert_eq!(
        selected
            .status
            .auxiliary_vision_model
            .as_ref()
            .map(assistant_protocol::ModelKey::as_str),
        Some("fixture")
    );
    assert!(selected.models[0].supports_image_input);

    let cleared = runtime
        .set_auxiliary_vision_model(assistant_protocol::SetAuxiliaryVisionModelRequest {
            model_key: None,
            expected_revision: selected.status.revision.expect("revision"),
        })
        .await
        .expect("clear auxiliary vision model");
    assert!(cleared.status.auxiliary_vision_model.is_none());
    assert!(
        !source
            .document
            .lock()
            .expect("source lock")
            .as_deref()
            .expect("document")
            .contains("[agent.vision]")
    );
}
