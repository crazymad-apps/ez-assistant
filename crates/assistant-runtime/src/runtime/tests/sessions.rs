use super::*;

#[tokio::test]
async fn creates_one_frozen_system_prompt_and_empty_conversation_per_session() {
    let system_prompt_factory = Arc::new(CountingSystemPromptFactory::new());
    let runtime = runtime_with_factories(
        Arc::new(StaticModelFactory::new(empty_model())),
        system_prompt_factory.clone(),
        ToolSetSnapshot::default(),
        32,
    );
    let first = runtime
        .create_session(CreateSessionRequest {
            title: Some("First".to_owned()),
            model_key: None,
            workspace_id: None,
        })
        .await
        .expect("first session");
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second session");

    assert_eq!(system_prompt_factory.created(), 2);
    assert_ne!(first.session.session_id, second.session.session_id);
    assert!(first.session.session_id.as_str().starts_with("s_"));
    assert_eq!(first.session.session_id.as_str().len(), 14);
    assert_eq!(first.session.title, "First");
    assert_eq!(second.session.title, "New Session");
    assert_eq!(first.session.message_count, 0);
    assert_eq!(second.session.message_count, 0);
    assert!(first.session.active_run_id.is_none());
    assert!(second.session.active_run_id.is_none());
    assert!(
        runtime
            .conversation_snapshot(&first.session.session_id)
            .await
            .expect("first conversation")
            .messages
            .is_empty()
    );
    assert!(
        runtime
            .conversation_snapshot(&second.session.session_id)
            .await
            .expect("second conversation")
            .messages
            .is_empty()
    );

    let first_prompt = runtime
        .session_for_test(&first.session.session_id)
        .system_prompt()
        .clone();
    let second_prompt = runtime
        .session_for_test(&second.session.session_id)
        .system_prompt()
        .clone();
    assert_ne!(first_prompt, second_prompt);
    assert_eq!(first.session.model_key.as_str(), "fixture");
}

#[tokio::test]
async fn list_and_get_are_deterministic_and_unknown_session_is_structured() {
    let runtime = runtime(empty_model());
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first session");
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second session");

    let listed = runtime
        .list_sessions(ListSessionsRequest::default())
        .expect("list sessions");
    let listed_ids = listed
        .sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<Vec<_>>();
    let mut expected_ids = vec![
        first.session.session_id.clone(),
        second.session.session_id.clone(),
    ];
    expected_ids.sort();
    assert_eq!(listed_ids, expected_ids);
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: second.session.session_id.clone(),
            })
            .expect("get session")
            .session,
        second.session
    );

    let missing = SessionId::new("missing").expect("session id");
    assert!(matches!(
        runtime.get_session(GetSessionRequest {
            session_id: missing.clone()
        }),
        Err(RuntimeError::SessionNotFound { session_id }) if session_id == missing
    ));
}

#[tokio::test]
async fn model_factory_failure_keeps_the_input_queued_without_appending_a_user_message() {
    let runtime = runtime_with_factories(
        Arc::new(FailingModelFactory),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        32,
    );
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session.session_id.clone(),
            message: "must not commit".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("input is durably accepted before model compilation");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    assert!(
        runtime
            .conversation_snapshot(&session.session.session_id)
            .await
            .expect("conversation")
            .messages
            .is_empty()
    );
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id
            })
            .expect("session")
            .session
            .queued_input_count,
        1
    );
}
