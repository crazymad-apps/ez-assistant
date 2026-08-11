use super::{store::FaultInjectingStore, *};

#[tokio::test]
async fn sessions_run_concurrently_and_cancellation_is_isolated_and_idempotent() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let runtime = hanging_runtime(2, Some("reused"), entered.clone(), cleanup);
    let mut events = runtime.subscribe_events();
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
            message: "first".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("first run");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("first run entered tool");
    let second_run = runtime
        .submit_input(SubmitInputRequest {
            session_id: second.session.session_id.clone(),
            message: "second".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("second run while first remains active");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("second run entered tool concurrently");

    let first_cancel = runtime
        .cancel_run(CancelRunRequest {
            session_id: first.session.session_id.clone(),
            run_id: first_run.run.run_id.clone(),
        })
        .await
        .expect("first cancellation");
    assert_eq!(
        first_cancel.run.status,
        assistant_protocol::RunStatus::Cancelling
    );
    let repeated = runtime
        .cancel_run(CancelRunRequest {
            session_id: first.session.session_id.clone(),
            run_id: first_run.run.run_id.clone(),
        })
        .await
        .expect("repeated cancellation");
    assert_eq!(repeated.run, first_cancel.run);
    let first_terminal =
        wait_for_terminal(&runtime, &first.session.session_id, &first_run.run.run_id).await;
    assert_eq!(
        first_terminal.status,
        assistant_protocol::RunStatus::Cancelled
    );
    assert_eq!(
        runtime
            .cancel_run(CancelRunRequest {
                session_id: first.session.session_id.clone(),
                run_id: first_run.run.run_id.clone(),
            })
            .await
            .expect("terminal cancellation is idempotent")
            .run,
        first_terminal
    );
    assert_eq!(
        runtime
            .get_run(GetRunRequest {
                session_id: second.session.session_id.clone(),
                run_id: second_run.run.run_id.clone(),
            })
            .await
            .expect("second run query")
            .run
            .status,
        assistant_protocol::RunStatus::Running
    );

    let reused = runtime
        .submit_input(SubmitInputRequest {
            session_id: first.session.session_id.clone(),
            message: "reuse first session".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("cancelled session is reusable while another session runs");
    assert_eq!(
        wait_for_terminal(&runtime, &first.session.session_id, &reused.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    runtime
        .cancel_run(CancelRunRequest {
            session_id: second.session.session_id.clone(),
            run_id: second_run.run.run_id.clone(),
        })
        .await
        .expect("second run cleanup cancellation");
    assert_eq!(
        wait_for_terminal(&runtime, &second.session.session_id, &second_run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );

    let mut observed = Vec::new();
    while let Ok(event) = events.try_recv() {
        observed.push(event);
    }
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(
                event,
                RuntimeEvent::RunCancelling { run_id, .. }
                    if run_id == &first_run.run.run_id
            ))
            .count(),
        1
    );
    for run_id in [
        &first_run.run.run_id,
        &second_run.run.run_id,
        &reused.run.run_id,
    ] {
        assert_eq!(
            observed
                .iter()
                .filter(|event| matches!(
                    event,
                    RuntimeEvent::RunFinished {
                        run_id: finished,
                        ..
                    } if finished == run_id
                ))
                .count(),
            1
        );
    }
    let missing = RunId::new("r_missing").expect("run id");
    assert!(matches!(
        runtime.cancel_run(CancelRunRequest {
            session_id: first.session.session_id,
            run_id: missing.clone(),
        }).await,
        Err(RuntimeError::RunNotFound { run_id, .. }) if run_id == missing
    ));
}

#[tokio::test]
async fn shutdown_cancels_active_runs_waits_for_settlement_and_is_idempotent() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let runtime = hanging_runtime(2, None, entered.clone(), cleanup);
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first session");
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second session");
    let mut runs = Vec::new();
    for session_id in [&first.session.session_id, &second.session.session_id] {
        let run = runtime
            .submit_input(SubmitInputRequest {
                session_id: session_id.clone(),
                message: "hang".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await
            .expect("run accepted");
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("run entered tool");
        runs.push((session_id.clone(), run.run.run_id));
    }

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.shutdown(ShutdownRuntimeRequest::default()),
    )
    .await
    .expect("shutdown completes")
    .expect("shutdown succeeds");
    assert_eq!(result.lifecycle, RuntimeLifecycle::Stopped);
    assert_eq!(
        runtime.lifecycle().expect("lifecycle"),
        RuntimeLifecycle::Stopped
    );
    for (session_id, run_id) in &runs {
        let snapshot = runtime
            .get_run(GetRunRequest {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
            })
            .await
            .expect("settled run")
            .run;
        assert_eq!(snapshot.status, assistant_protocol::RunStatus::Cancelled);
        assert!(
            runtime
                .get_session(GetSessionRequest {
                    session_id: session_id.clone(),
                })
                .expect("session")
                .session
                .active_run_id
                .is_none()
        );
    }
    assert!(matches!(
        runtime
            .create_session(CreateSessionRequest::default())
            .await,
        Err(RuntimeError::RuntimeNotRunning {
            lifecycle: RuntimeLifecycle::Stopped
        })
    ));
    assert_eq!(
        runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await
            .expect("repeated shutdown")
            .lifecycle,
        RuntimeLifecycle::Stopped
    );
}

#[tokio::test]
async fn shutdown_timeout_aborts_supervisor_and_force_settles_active_run() {
    let entered = Arc::new(Notify::new());
    let model = Arc::new(EnteredNeverModel {
        capabilities: model_capabilities(false),
        entered: entered.clone(),
    });
    let runtime = runtime_with_factories_and_config(
        Arc::new(StaticModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity"))
            .with_shutdown_timeout(Duration::from_millis(10)),
    );
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let run = runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session.session_id.clone(),
            message: "never completes".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("model entered");

    let shutdown = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.shutdown(ShutdownRuntimeRequest::default()),
    )
    .await
    .expect("bounded shutdown")
    .expect("shutdown result");
    assert_eq!(shutdown.lifecycle, RuntimeLifecycle::Stopped);

    let snapshot = runtime
        .get_run(GetRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: run.run.run_id,
        })
        .await
        .expect("forced settlement")
        .run;
    assert_eq!(snapshot.status, assistant_protocol::RunStatus::Failed);
    assert_eq!(
        snapshot.error.expect("internal failure").code,
        assistant_protocol::RuntimeErrorCode::Internal
    );
    assert!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id,
            })
            .expect("session snapshot")
            .session
            .active_run_id
            .is_none()
    );
}

#[tokio::test]
async fn storage_shutdown_timeout_is_bounded_and_observable() {
    let store = Arc::new(FaultInjectingStore::hang_shutdown());
    let runtime = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity"))
            .with_shutdown_timeout(Duration::from_millis(10)),
    )
    .await;

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.shutdown(ShutdownRuntimeRequest::default()),
    )
    .await
    .expect("shutdown timeout is bounded")
    .expect_err("hanging store must fail shutdown");
    assert!(matches!(result, RuntimeError::StorageUnavailable { .. }));
    assert!(store.shutdown_called());
    assert_eq!(
        runtime.lifecycle().expect("lifecycle"),
        RuntimeLifecycle::ShuttingDown
    );
}

#[tokio::test]
async fn force_settlement_failure_still_shuts_down_store() {
    let entered = Arc::new(Notify::new());
    let model = Arc::new(EnteredNeverModel {
        capabilities: model_capabilities(false),
        entered: entered.clone(),
    });
    let store = Arc::new(FaultInjectingStore::fail_settlement());
    let runtime = runtime_with_store(
        model,
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity"))
            .with_shutdown_timeout(Duration::from_millis(10)),
    )
    .await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session.session_id,
            message: "never completes".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("run");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("model entered");

    assert!(matches!(
        runtime.shutdown(ShutdownRuntimeRequest::default()).await,
        Err(RuntimeError::StorageUnavailable { .. })
    ));
    assert!(store.shutdown_called());
    assert_eq!(
        runtime.lifecycle().expect("lifecycle"),
        RuntimeLifecycle::Stopped
    );
}

#[tokio::test]
async fn start_and_shutdown_race_has_no_untracked_active_run() {
    let final_message = assistant_text("assistant-final", "done");
    let model = Arc::new(ScriptedModelService::completing(
        model_capabilities(false),
        8_192,
        final_message,
    ));
    let runtime = Arc::new(runtime(model));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let barrier = Arc::new(Barrier::new(3));
    let start_runtime = runtime.clone();
    let start_barrier = barrier.clone();
    let session_id = session.session.session_id.clone();
    let start = tokio::spawn(async move {
        start_barrier.wait().await;
        start_runtime
            .submit_input(SubmitInputRequest {
                session_id,
                message: "race".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await
    });
    let shutdown_runtime = runtime.clone();
    let shutdown_barrier = barrier.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_barrier.wait().await;
        shutdown_runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await
    });
    barrier.wait().await;

    let start_result = start.await.expect("start task");
    assert_eq!(
        shutdown
            .await
            .expect("shutdown task")
            .expect("shutdown result")
            .lifecycle,
        RuntimeLifecycle::Stopped
    );
    match start_result {
        Ok(started) => {
            assert!(
                runtime
                    .get_run(GetRunRequest {
                        session_id: session.session.session_id.clone(),
                        run_id: started.run.run_id,
                    })
                    .await
                    .expect("accepted run was tracked")
                    .run
                    .status
                    .is_terminal()
            );
        }
        Err(RuntimeError::RuntimeNotRunning { .. }) => {}
        Err(error) => panic!("unexpected start result: {error}"),
    }
    assert!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id,
            })
            .expect("session")
            .session
            .active_run_id
            .is_none()
    );
}
