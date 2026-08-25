use super::*;

use assistant_protocol::{
    AgentVariant, ApprovalMode, CancelQueuedInputRequest, IdempotencyKey, RetryRunRequest,
    SetSessionApprovalModeRequest, SubmitInputRequest,
};

#[tokio::test]
async fn repeated_idempotency_key_returns_the_first_input_and_run() {
    let runtime = runtime_with_tools(
        Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            [ModelScript::Events(message_events(&assistant_text(
                "a-1", "done",
            )))],
        )),
        ToolSetSnapshot::default(),
    );
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let key = IdempotencyKey::new("submit-1").expect("key");
    let first = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: AgentVariant::Plan,
            session_id: session.session.session_id.clone(),
            message: "first payload".to_owned(),
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: Some(key.clone()),
        })
        .await
        .expect("first submit");
    let repeated = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "different payload is ignored for the same key".to_owned(),
            attachment_ids: Vec::new(),
            skill_name: Some("INVALID-SKILL-NAME".to_owned()),
            idempotency_key: Some(key),
        })
        .await
        .expect("idempotent retry");
    assert_eq!(repeated.input_id, first.input_id);
    assert_eq!(repeated.run.run_id, first.run.run_id);
    assert_eq!(repeated.run.variant, AgentVariant::Plan);
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone(),
            })
            .expect("session summary")
            .session
            .current_variant,
        AgentVariant::Plan,
        "a duplicate key must not apply the repeated request variant"
    );
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &first.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("conversation");
    assert_eq!(conversation.messages.len(), 2);
    let ConversationMessage::User(user) = &conversation.messages[0] else {
        panic!("first conversation message must be user")
    };
    assert!(matches!(user.parts[0], UserPart::Text(_)));
    let UserPart::InternalContext(injected) = &user.parts[1] else {
        panic!("variant context must follow user-visible parts")
    };
    assert_eq!(injected.text, crate::agent_variant::PLAN_INJECTION_V1);
}

#[tokio::test]
async fn queued_input_can_be_cancelled_without_entering_the_conversation() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let runtime = hanging_runtime(1, None, entered.clone(), cleanup.clone());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let active = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "active".to_owned(),
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("active");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("tool entered");
    let queued = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "queued".to_owned(),
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("queued");
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone()
            })
            .expect("summary")
            .session
            .queued_input_count,
        1
    );
    runtime
        .cancel_queued_input(CancelQueuedInputRequest {
            session_id: session.session.session_id.clone(),
            input_id: queued.input_id,
        })
        .await
        .expect("cancel queued");
    runtime
        .cancel_run(CancelRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: active.run.run_id.clone(),
        })
        .await
        .expect("cancel active");
    tokio::time::timeout(Duration::from_secs(1), cleanup.notified())
        .await
        .expect("cleanup");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &active.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone()
            })
            .expect("summary")
            .session
            .queued_input_count,
        0
    );
    assert!(
        runtime
            .get_run(GetRunRequest {
                session_id: session.session.session_id,
                run_id: queued.run.run_id
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn same_session_inputs_execute_in_acceptance_order() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let runtime = hanging_runtime(
        1,
        Some("second completed"),
        entered.clone(),
        cleanup.clone(),
    );
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    set_auto_approval(&runtime, &session.session.session_id).await;
    let first = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "first".to_owned(),
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("first");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("first entered");
    let second = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "second".to_owned(),
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("second queued");
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone()
            })
            .expect("summary")
            .session
            .queued_input_count,
        1
    );
    runtime
        .cancel_run(CancelRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: first.run.run_id.clone(),
        })
        .await
        .expect("cancel first");
    tokio::time::timeout(Duration::from_secs(1), cleanup.notified())
        .await
        .expect("first cleanup");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &first.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &second.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("conversation");
    let user_texts = conversation
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::User(message) => {
                message.parts.iter().find_map(|part| match part {
                    UserPart::Text(part) => Some(part.text.as_str()),
                    UserPart::Injected(_)
                    | UserPart::InternalContext(_)
                    | UserPart::FileReferences(_) => None,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_texts, ["first", "second"]);
}

#[tokio::test]
async fn retrying_a_prestart_failure_reuses_the_user_message_and_creates_a_new_attempt() {
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG.to_owned()));
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "a-1",
            "recovered",
        )))],
    ));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source,
        Arc::new(FailOnceModelFactory::new(model)),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("load config");
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let first = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "retry me".to_owned(),
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("accepted");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &first.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    assert!(
        runtime
            .conversation_snapshot(&session.session.session_id)
            .await
            .expect("empty conversation")
            .messages
            .is_empty()
    );
    runtime
        .set_session_approval_mode(SetSessionApprovalModeRequest {
            session_id: session.session.session_id.clone(),
            approval_mode: ApprovalMode::Auto,
        })
        .await
        .expect("change approval mode");
    let retry = runtime
        .retry_run(RetryRunRequest {
            session_id: session.session.session_id.clone(),
            run_id: first.run.run_id,
        })
        .await
        .expect("retry");
    assert_eq!(retry.run.input_id, first.input_id);
    assert_eq!(retry.run.attempt, 2);
    assert_eq!(retry.run.variant, AgentVariant::Build);
    assert_eq!(retry.run.approval_mode, ApprovalMode::Auto);
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &retry.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
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
