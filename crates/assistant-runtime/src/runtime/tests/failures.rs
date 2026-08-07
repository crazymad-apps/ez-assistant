use super::*;

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
        .expect("session");

    let failed = runtime
        .start_run(StartRunRequest {
            session_id: session.session.session_id.clone(),
            message: "fail once".to_owned(),
        })
        .expect("failed run accepted");
    let failed = wait_for_terminal(&runtime, &session.session.session_id, &failed.run.run_id).await;
    assert_eq!(failed.status, assistant_protocol::RunStatus::Failed);
    assert!(failed.error.is_some());

    let compact = runtime
        .start_run(StartRunRequest {
            session_id: session.session.session_id.clone(),
            message: "overflow once".to_owned(),
        })
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
            .expect("conversation")
            .messages
            .len(),
        2
    );
}

#[tokio::test]
async fn completion_panic_is_caught_and_session_is_not_left_busy() {
    let model = Arc::new(PanicModel {
        capabilities: model_capabilities(false),
    });
    let runtime = runtime(model);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .expect("session");
    let started = runtime
        .start_run(StartRunRequest {
            session_id: session.session.session_id.clone(),
            message: "panic".to_owned(),
        })
        .expect("run accepted");

    let terminal =
        wait_for_terminal(&runtime, &session.session.session_id, &started.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Failed);
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
async fn blank_message_and_unknown_run_do_not_mutate_conversation() {
    let runtime = runtime(empty_model());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .expect("session");
    assert!(matches!(
        runtime.start_run(StartRunRequest {
            session_id: session.session.session_id.clone(),
            message: " \n\t".to_owned(),
        }),
        Err(RuntimeError::InvalidRequest { .. })
    ));
    assert!(
        runtime
            .conversation_snapshot(&session.session.session_id)
            .expect("conversation")
            .messages
            .is_empty()
    );
    let missing = RunId::new("r_missing").expect("run id");
    assert!(matches!(
        runtime.get_run(GetRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: missing.clone(),
        }),
        Err(RuntimeError::RunNotFound { run_id, .. }) if run_id == missing
    ));
}
