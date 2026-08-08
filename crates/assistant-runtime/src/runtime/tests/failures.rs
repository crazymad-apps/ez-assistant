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
async fn failed_and_compaction_runs_settle_without_automatic_retry() {
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
            session_id: session.session.session_id.clone(),
            message: "fail once".to_owned(),
            idempotency_key: None,
        })
        .await
        .expect("failed run accepted");
    let failed = wait_for_terminal(&runtime, &session.session.session_id, &failed.run.run_id).await;
    assert_eq!(failed.status, assistant_protocol::RunStatus::Failed);
    assert!(failed.error.is_some());

    let compact = runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session.session_id.clone(),
            message: "overflow once".to_owned(),
            idempotency_key: None,
        })
        .await
        .expect("compaction run accepted");
    let compact =
        wait_for_terminal(&runtime, &session.session.session_id, &compact.run.run_id).await;
    assert_eq!(
        compact.status,
        assistant_protocol::RunStatus::CompactionRequired
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
            session_id: session.session.session_id.clone(),
            message: "panic".to_owned(),
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
            session_id: session.session.session_id.clone(),
            message: "first".to_owned(),
            idempotency_key: None,
        })
        .await
        .expect("first input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("first model entered");
    runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session.session_id.clone(),
            message: "must remain queued".to_owned(),
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
                session_id: session.session.session_id,
                message: "must be rejected".to_owned(),
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
                session_id: session.session.session_id.clone(),
                message: " \n\t".to_owned(),
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
