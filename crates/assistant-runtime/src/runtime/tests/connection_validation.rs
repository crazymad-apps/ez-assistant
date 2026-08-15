use super::*;

#[tokio::test]
async fn connection_validation_uses_only_the_fixed_minimal_request_and_creates_no_session() {
    let model = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        assistant_text("validation-response", "OK"),
    ));
    let system_prompt_factory = Arc::new(CountingSystemPromptFactory::new());
    let runtime = runtime_with_factories(
        Arc::new(StaticModelFactory::new(model.clone())),
        system_prompt_factory.clone(),
        ToolSetSnapshot::default(),
        32,
    );

    let result = runtime
        .validate_model_connection(configured_validation_request())
        .await
        .expect("validation result");
    assert_eq!(result.outcome, ConnectionValidationOutcome::Succeeded);
    assert_eq!(system_prompt_factory.created(), 0);
    assert!(
        runtime
            .list_sessions(ListSessionsRequest::default())
            .expect("sessions")
            .sessions
            .is_empty()
    );

    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.system.is_empty());
    assert!(request.tools.is_empty());
    assert_eq!(request.tool_choice, ToolChoice::Auto);
    assert_eq!(
        request.generation,
        GenerationConfig {
            temperature: None,
            top_p: None,
            max_output_tokens: Some(CONNECTION_VALIDATION_MAX_OUTPUT_TOKENS),
            stop: Vec::new(),
        }
    );
    assert!(request.reasoning.is_none());
    assert!(request.provider_options.is_empty());
    assert_eq!(request.conversation.messages.len(), 1);
    let ConversationMessage::User(message) = &request.conversation.messages[0] else {
        panic!("validation request must contain one user message");
    };
    assert_eq!(message.parts.len(), 1);
    assert!(matches!(
        &message.parts[0],
        UserPart::Text(part) if part.text == CONNECTION_VALIDATION_PROMPT
    ));
}

#[tokio::test]
async fn candidate_connection_validation_uses_unsaved_form_values_without_persisting_them() {
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG.to_owned()));
    let model: Arc<dyn ModelService> = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        assistant_text("validation-response", "OK"),
    ));
    let factory = Arc::new(RecordingModelFactory::new([model]));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source.clone(),
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("initial reload");
    let original = source.document.lock().expect("source lock").clone();

    let result = runtime
        .validate_model_connection(ValidateModelConnectionRequest {
            target: ModelConnectionTarget::Candidate(model_input(
                "candidate",
                "https://api.example.test/v1",
                assistant_protocol::ModelCredentialChange::Replace(
                    assistant_protocol::SecretValue::new("candidate-secret".to_owned()),
                ),
            )),
        })
        .await
        .expect("candidate validation result");
    assert_eq!(result.outcome, ConnectionValidationOutcome::Succeeded);
    assert_eq!(factory.api_keys(), ["candidate-secret"]);
    assert_eq!(*source.document.lock().expect("source lock"), original);
}

#[tokio::test]
async fn deepseek_connection_validation_injects_only_its_required_profile_options() {
    let model = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        assistant_text("validation-response", "anything"),
    ));
    let runtime = runtime_with_factories(
        Arc::new(StaticModelFactory::new(model.clone())),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        32,
    );
    runtime.config_registry.replace_document_for_test(
        &TEST_CONFIG
            .replace("provider = \"fixture\"", "provider = \"deepseek\"")
            .replace("max_output_tokens = 4096", "max_output_tokens = 8"),
    );

    let result = runtime
        .validate_model_connection(configured_validation_request())
        .await
        .expect("validation result");
    assert_eq!(result.outcome, ConnectionValidationOutcome::Succeeded);
    let request = model.take_requests().pop().expect("captured request");
    assert_eq!(request.reasoning, Some(ReasoningConfig { effort: None }));
    assert_eq!(
        request.provider_options.get("deepseek"),
        Some(&serde_json::json!({"thinking": {"type": "enabled"}}))
    );
    assert_eq!(request.generation.temperature, None);
    assert_eq!(request.generation.top_p, None);
    assert_eq!(request.generation.max_output_tokens, Some(8));
    assert!(request.generation.stop.is_empty());
}

#[tokio::test]
async fn connection_validation_maps_structured_model_failures_without_exposing_messages() {
    let cases = [
        (
            ModelError::Config("secret config detail".to_owned()),
            ConnectionValidationFailureKind::Configuration,
        ),
        (
            ModelError::Transport {
                kind: ModelTransportErrorKind::Connection,
                message: "secret connection detail".to_owned(),
            },
            ConnectionValidationFailureKind::Connection,
        ),
        (
            ModelError::Transport {
                kind: ModelTransportErrorKind::Timeout,
                message: "secret timeout detail".to_owned(),
            },
            ConnectionValidationFailureKind::Timeout,
        ),
        (
            ModelError::Auth("secret auth detail".to_owned()),
            ConnectionValidationFailureKind::Authentication,
        ),
        (
            ModelError::Provider {
                message: "secret provider detail".to_owned(),
                status: Some(404),
            },
            ConnectionValidationFailureKind::ModelUnavailable,
        ),
        (
            ModelError::RateLimited {
                message: "secret rate detail".to_owned(),
                retry_after_ms: Some(1),
            },
            ConnectionValidationFailureKind::RateLimited,
        ),
        (
            ModelError::Unavailable {
                message: "secret unavailable detail".to_owned(),
                status: Some(503),
                retry_after_ms: None,
            },
            ConnectionValidationFailureKind::ServiceUnavailable,
        ),
        (
            ModelError::Provider {
                message: "secret rejection detail".to_owned(),
                status: Some(422),
            },
            ConnectionValidationFailureKind::ProviderRejected,
        ),
        (
            ModelError::Protocol("secret protocol detail".to_owned()),
            ConnectionValidationFailureKind::Protocol,
        ),
    ];

    for (error, expected) in cases {
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            [ModelScript::FailEstablishment(error)],
        ));
        let runtime = runtime(model);
        let result = runtime
            .validate_model_connection(configured_validation_request())
            .await
            .expect("classified validation result");
        let ConnectionValidationOutcome::Failed(failure) = result.outcome else {
            panic!("validation must fail");
        };
        assert_eq!(failure.kind, expected);
        assert!(!failure.message.contains("secret"));
    }

    let runtime = runtime_with_factories(
        Arc::new(FailingModelFactory),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        32,
    );
    let result = runtime
        .validate_model_connection(configured_validation_request())
        .await
        .expect("factory failure is a validation result");
    assert!(matches!(
        result.outcome,
        ConnectionValidationOutcome::Failed(ConnectionValidationFailure {
            kind: ConnectionValidationFailureKind::Configuration,
            ..
        })
    ));
}

#[tokio::test]
async fn connection_validation_reuses_retry_policy_and_rejects_malformed_streams() {
    let retry_model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::FailEstablishment(ModelError::Transport {
                kind: ModelTransportErrorKind::Connection,
                message: "temporary connection failure".to_owned(),
            }),
            ModelScript::Events(message_events(&assistant_text("validation-response", "OK"))),
        ],
    ));
    let retry_runtime = runtime(retry_model.clone());
    retry_runtime.config_registry.replace_document_for_test(
        &TEST_CONFIG.replace(
            "default_model = \"fixture\"",
            "default_model = \"fixture\"\n\n[runtime.model_retry]\nretry_on = [\"connection\"]\ndelays_ms = [1]\nmax_retry_after_ms = 10",
        ),
    );
    let retried = retry_runtime
        .validate_model_connection(configured_validation_request())
        .await
        .expect("retried validation");
    assert_eq!(retried.outcome, ConnectionValidationOutcome::Succeeded);
    assert_eq!(retry_model.take_requests().len(), 2);

    let malformed_model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(Vec::new())],
    ));
    let malformed = runtime(malformed_model)
        .validate_model_connection(configured_validation_request())
        .await
        .expect("malformed stream result");
    assert!(matches!(
        malformed.outcome,
        ConnectionValidationOutcome::Failed(ConnectionValidationFailure {
            kind: ConnectionValidationFailureKind::Protocol,
            ..
        })
    ));
}

#[tokio::test]
async fn connection_validation_enforces_timeout_and_shutdown_cancellation() {
    let timeout_runtime = runtime(Arc::new(NeverModel {
        capabilities: model_capabilities(false),
    }));
    timeout_runtime.config_registry.replace_document_for_test(
        &TEST_CONFIG.replace(
            "default_model = \"fixture\"",
            "default_model = \"fixture\"\n\n[runtime.model_transport]\nconnect_timeout_ms = 1\nrequest_timeout_ms = 10",
        ),
    );
    let timed_out = tokio::time::timeout(
        Duration::from_secs(1),
        timeout_runtime.validate_model_connection(configured_validation_request()),
    )
    .await
    .expect("runtime timeout completes")
    .expect("timeout is validation result");
    assert!(matches!(
        timed_out.outcome,
        ConnectionValidationOutcome::Failed(ConnectionValidationFailure {
            kind: ConnectionValidationFailureKind::Timeout,
            ..
        })
    ));

    let entered = Arc::new(Notify::new());
    let runtime = Arc::new(runtime(Arc::new(CancellationAwareModel {
        capabilities: model_capabilities(false),
        entered: entered.clone(),
    })));
    let validating = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            runtime
                .validate_model_connection(configured_validation_request())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("validation entered model");
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), validating)
            .await
            .expect("validation cancellation completes")
            .expect("validation task"),
        Err(RuntimeError::RuntimeNotRunning { lifecycle })
            if lifecycle == RuntimeLifecycle::ShuttingDown
                || lifecycle == RuntimeLifecycle::Stopped
    ));
}
