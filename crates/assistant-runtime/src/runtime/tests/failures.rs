use std::sync::atomic::{AtomicUsize, Ordering};

use agent_model::{ModelEvent, ModelEventStream};

use super::{store::FaultInjectingStore, *};

struct GatedCompletionModel {
    capabilities: ModelCapabilities,
    events: Vec<ModelEvent>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    calls: AtomicUsize,
}

impl ModelService for GatedCompletionModel {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        8_192
    }

    fn stream(&self, _request: ModelRequest, _context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let events = self.events.clone();
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
        })
    }
}

#[tokio::test]
async fn failed_run_settles_and_uncompressible_overflow_reports_compaction_failure() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::FailEstablishment(ModelError::Provider {
                message: "fixture failure".to_owned(),
                status: Some(500),
            }),
            ModelScript::FailEstablishment(ModelError::ContextOverflow {
                message: "fixture overflow".to_owned(),
            }),
        ],
    ));
    let runtime = runtime(model.clone());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");

    let failed = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "fail once".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("failed run accepted");
    let failed = wait_for_terminal(&runtime, &session.session.session_id, &failed.run.run_id).await;
    assert_eq!(failed.status, assistant_protocol::RunStatus::Failed);
    assert!(failed.error.is_some());

    let compact = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "overflow once".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("compaction run accepted");
    let compact =
        wait_for_terminal(&runtime, &session.session.session_id, &compact.run.run_id).await;
    assert_eq!(compact.status, assistant_protocol::RunStatus::Failed);
    assert_eq!(
        compact.error.as_ref().map(|error| error.code),
        Some(assistant_protocol::RuntimeErrorCode::ContextCompactionFailed)
    );
    assert!(
        compact
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("provider_overflow"))
    );
    assert_eq!(model.take_requests().len(), 2);
    assert_eq!(
        runtime
            .conversation_snapshot(&session.session.session_id)
            .await
            .expect("conversation")
            .messages
            .len(),
        2
    );
}

#[tokio::test]
async fn threshold_compaction_replaces_history_and_continues_the_same_run() {
    let mut first = assistant_text("assistant-before-compaction", "long prior answer");
    first.usage = Some(agent_types::TokenUsage {
        input_tokens: 6_500,
        output_tokens: 500,
        total_tokens: 7_000,
        cached_input_tokens: Some(6_000),
        reasoning_tokens: None,
    });
    let summary = assistant_text("summary-generated", "prior request and answer summarized");
    let final_message = assistant_text("assistant-after-compaction", "continued after compaction");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&first)),
            ModelScript::Events(message_events(&summary)),
            ModelScript::Events(message_events(&final_message)),
        ],
    ));
    let runtime = runtime(model.clone());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");

    let first_run = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "first turn".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("first run accepted");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &first_run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    let continued = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "continue".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("continuation accepted");
    let continued =
        wait_for_terminal(&runtime, &session.session.session_id, &continued.run.run_id).await;
    assert_eq!(continued.status, assistant_protocol::RunStatus::Completed);

    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("compacted conversation");
    agent_context::ContextLayout::build(&conversation)
        .expect("compacted parent conversation remains structurally valid");
    assert!(matches!(
        conversation.messages.first(),
        Some(ConversationMessage::ContextSummary(summary))
            if summary.text == "prior request and answer summarized"
    ));
    assert!(matches!(
        conversation.messages.last(),
        Some(ConversationMessage::Assistant(message))
            if message.id.as_str() == "assistant-after-compaction"
    ));
    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].tool_choice, ToolChoice::None);
}

#[tokio::test]
async fn retry_exhaustion_persists_safe_attempt_diagnostics() {
    const PRIVATE_PROVIDER_DETAIL: &str = "private-provider-overload-detail";
    let failure = || ModelError::Unavailable {
        message: PRIVATE_PROVIDER_DETAIL.to_owned(),
        status: Some(503),
        retry_after_ms: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        (0..4).map(|_| ModelScript::FailEstablishment(failure())),
    ));
    let runtime = runtime(model.clone());
    runtime.config_registry.replace_document_for_test(&format!(
        "{TEST_CONFIG}\n[runtime.model_retry]\nretry_on = [\"unavailable\"]\ndelays_ms = [1, 1, 1]\nmax_retry_after_ms = 10\n"
    ));
    let mut events = runtime.subscribe_events();
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "retry unavailable model".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    let error = terminal.error.expect("model failure");
    assert_eq!(
        error.code,
        assistant_protocol::RuntimeErrorCode::ModelExecutionFailed
    );
    assert_eq!(
        error.message,
        "model execution failed before stream establishment (kind=service_unavailable, attempts=4, retries=3, output_observed=false)"
    );
    assert!(!error.message.contains(PRIVATE_PROVIDER_DETAIL));
    assert_eq!(model.take_requests().len(), 4);

    let observed = tokio::time::timeout(Duration::from_secs(1), async {
        let mut observed = Vec::new();
        loop {
            let event = events.recv().await.expect("runtime event");
            let finished = matches!(
                &event,
                RuntimeEvent::RunFinished { run_id, .. } if run_id == &started.run.run_id
            );
            observed.push(event);
            if finished {
                return observed;
            }
        }
    })
    .await
    .expect("terminal event");
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::ModelAttemptStarted { .. }))
            .count(),
        4
    );
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::ModelRetryScheduled { .. }))
            .count(),
        3
    );
    assert!(observed.iter().any(|event| matches!(
        event,
        RuntimeEvent::ModelAttemptFailed {
            attempt: 4,
            kind: assistant_protocol::ModelFailureKind::ServiceUnavailable,
            will_retry: false,
            ..
        }
    )));
}

#[tokio::test]
async fn in_stream_failure_reports_establishment_and_partial_output_without_retry() {
    const PRIVATE_STREAM_DETAIL: &str = "private-stream-disconnect-detail";
    let message_id = MessageId::new("stream-failure-message").expect("message id");
    let part_id = PartId::new("stream-failure-text").expect("part id");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(vec![
            ModelEvent::TurnStarted {
                message_id,
                model: ModelIdentity::new(
                    ProviderId::new("fixture").expect("provider id"),
                    "fixture-model",
                ),
            },
            ModelEvent::TextStarted {
                id: part_id.clone(),
            },
            ModelEvent::TextDelta {
                id: part_id,
                delta: "partial".to_owned(),
            },
            ModelEvent::TurnFailed {
                error: ModelError::Transport {
                    kind: ModelTransportErrorKind::Interrupted,
                    message: PRIVATE_STREAM_DETAIL.to_owned(),
                },
            },
        ])],
    ));
    let runtime = runtime(model.clone());
    runtime.config_registry.replace_document_for_test(&format!(
        "{TEST_CONFIG}\n[runtime.model_retry]\nretry_on = [\"connection\", \"timeout\", \"rate_limited\", \"unavailable\"]\ndelays_ms = [1, 1, 1]\nmax_retry_after_ms = 10\n"
    ));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "stream then fail".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    let error = terminal.error.expect("model failure");
    assert_eq!(
        error.code,
        assistant_protocol::RuntimeErrorCode::ModelExecutionFailed
    );
    assert_eq!(
        error.message,
        "model execution failed after stream establishment (kind=stream_interrupted, attempts=1, retries=0, output_observed=true)"
    );
    assert!(!error.message.contains(PRIVATE_STREAM_DETAIL));
    assert_eq!(model.take_requests().len(), 1);
}

#[tokio::test]
async fn core_engine_panic_becomes_internal_failure_and_session_is_not_left_busy() {
    let model = Arc::new(PanicModel {
        capabilities: model_capabilities(false),
    });
    let runtime = runtime(model);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let started = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "panic".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Failed);
    let error = terminal.error.expect("internal failure");
    assert_eq!(error.code, assistant_protocol::RuntimeErrorCode::Internal);
    assert_eq!(
        error.message,
        "agent execution task terminated unexpectedly"
    );
    assert!(!error.message.contains("private model panic payload"));
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id,
            })
            .expect("session query")
            .session
            .active_run_id,
        None
    );
}

#[tokio::test]
async fn runtime_task_panic_settles_current_run_and_faults_session_without_waking_queue() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let model = Arc::new(GatedCompletionModel {
        capabilities: model_capabilities(false),
        events: message_events(&assistant_text("assistant-final", "done")),
        entered: entered.clone(),
        release: release.clone(),
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(FaultInjectingStore::panic_once_on_settlement());
    let runtime = runtime_with_store(
        model.clone(),
        store,
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
    )
    .await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let first = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "first".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("first input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("first model entered");
    runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "must remain queued".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("second input queued");
    release.notify_one();

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &first.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Failed);
    let summary = runtime
        .get_session(GetSessionRequest {
            session_id: session.session.session_id.clone(),
        })
        .expect("session summary")
        .session;
    assert_eq!(summary.active_run_id, None);
    assert_eq!(summary.queued_input_count, 1);
    assert_eq!(model.calls.load(Ordering::Relaxed), 1);
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session.session.session_id,
                message: "must be rejected".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::SessionFaulted { .. })
    ));
}

#[tokio::test]
async fn blank_message_and_unknown_run_do_not_mutate_conversation() {
    let runtime = runtime(empty_model());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session.session.session_id.clone(),
                message: " \n\t".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
    assert!(
        runtime
            .conversation_snapshot(&session.session.session_id)
            .await
            .expect("conversation")
            .messages
            .is_empty()
    );
    let missing = RunId::new("r_missing").expect("run id");
    assert!(matches!(
        runtime.get_run(GetRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: missing.clone(),
        }).await,
        Err(RuntimeError::RunNotFound { run_id, .. }) if run_id == missing
    ));
}
