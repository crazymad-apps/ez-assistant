use super::*;

#[tokio::test]
async fn parent_update_plan_is_automatic_durable_and_injected_at_next_claim() {
    let update = AssistantMessage {
        id: MessageId::new("assistant-update-plan").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("work-plan-call-1").expect("tool call id"),
            name: ToolName::new("update_plan").expect("tool name"),
            arguments: json!({
                "objective": "deliver M1",
                "items": [{
                    "text": "verify persistence",
                    "status": "in_progress"
                }]
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let tolerant_update = AssistantMessage {
        id: MessageId::new("assistant-update-plan-without-ids").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("work-plan-call-2").expect("tool call id"),
            name: ToolName::new("update_plan").expect("tool name"),
            arguments: json!({
                "items": [{
                    "id": "todo-stale-model-copy",
                    "text": "verify persistence",
                    "status": "pending"
                }]
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&update)),
            ModelScript::Events(message_events(&assistant_text(
                "assistant-plan-saved",
                "plan saved",
            ))),
            ModelScript::Events(message_events(&tolerant_update)),
            ModelScript::Events(message_events(&assistant_text(
                "assistant-next-input",
                "next input handled",
            ))),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
    )
    .await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;

    let first = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "make a plan".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("submit first input");
    let first = wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    assert_eq!(first.status, assistant_protocol::RunStatus::Completed);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session_id.clone(),
            })
            .expect("list approvals")
            .approvals
            .is_empty()
    );
    let plan = store
        .load_work_plan(&session_id)
        .await
        .expect("load plan")
        .expect("plan exists");
    assert_eq!(plan.revision, 1);
    assert_eq!(plan.objective, "deliver M1");
    assert_eq!(plan.items.len(), 1);

    let second = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "continue from the plan".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("submit second input");
    let second = wait_for_terminal(&runtime, &session_id, &second.run.run_id).await;
    assert_eq!(second.status, assistant_protocol::RunStatus::Completed);

    let requests = model.take_requests();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "update_plan")
    );
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "update_goal")
    );
    let first_user = requests[0]
        .conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(message) => Some(message),
            _ => None,
        })
        .expect("first user");
    assert!(!first_user.parts.iter().any(|part| {
        matches!(part, UserPart::Injected(text) if text.text.starts_with(crate::work_plan::WORK_PLAN_CONTEXT_V1))
    }));
    let last_user = requests[2]
        .conversation
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ConversationMessage::User(message) => Some(message),
            _ => None,
        })
        .expect("latest user");
    let injected = last_user
        .parts
        .iter()
        .find_map(|part| match part {
            UserPart::Injected(text)
                if text
                    .text
                    .starts_with(crate::work_plan::WORK_PLAN_CONTEXT_V1) =>
            {
                Some(&text.text)
            }
            _ => None,
        })
        .expect("claim-time work plan context");
    assert!(injected.contains("deliver M1"));
    assert!(injected.contains("verify persistence"));
    assert!(!injected.contains(plan.items[0].id.as_str()));

    let persisted = store
        .load_conversation(&session_id)
        .await
        .expect("load conversation");
    let users = persisted
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::User(message) => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    assert!(!users[0].parts.iter().any(|part| {
        matches!(part, UserPart::Injected(text) if text.text.starts_with(crate::work_plan::WORK_PLAN_CONTEXT_V1))
    }));
    assert!(users[1].parts.iter().any(|part| {
        matches!(part, UserPart::Injected(text) if text.text.starts_with(crate::work_plan::WORK_PLAN_CONTEXT_V1))
    }));

    let updated_plan = store
        .load_work_plan(&session_id)
        .await
        .expect("load updated plan")
        .expect("updated plan exists");
    assert_eq!(updated_plan.revision, 2);
    assert_eq!(updated_plan.objective, "deliver M1");
    assert_eq!(updated_plan.items[0].id, plan.items[0].id);
    assert_eq!(
        updated_plan.items[0].status,
        crate::StoredTodoItemStatus::Pending
    );

    let view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value;
    let projected = view.work_plan.expect("projected work plan");
    assert_eq!(projected.revision, 2);
    assert_eq!(projected.objective, "deliver M1");
    assert_eq!(projected.items[0].id, plan.items[0].id);
    assert_eq!(
        projected.items[0].status,
        assistant_protocol::TodoItemStatusSnapshot::Pending
    );

    assert!(matches!(
        runtime
            .clear_work_plan(assistant_protocol::ClearWorkPlanRequest {
                session_id: session_id.clone(),
                expected_revision: 0,
            })
            .await,
        Err(RuntimeError::WorkPlanRevisionConflict { .. })
    ));
    let mut events = runtime.subscribe_events();
    runtime
        .clear_work_plan(assistant_protocol::ClearWorkPlanRequest {
            session_id: session_id.clone(),
            expected_revision: 2,
        })
        .await
        .expect("clear work plan");
    let changed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let assistant_protocol::RuntimeEvent::WorkPlanChanged {
                session_id: changed_session,
                revision,
            } = events.recv().await.expect("work plan event")
            {
                break (changed_session, revision);
            }
        }
    })
    .await
    .expect("work plan changed event");
    assert_eq!(changed, (session_id.clone(), 2));
    assert!(
        runtime
            .get_session_view(assistant_protocol::GetSessionViewRequest { session_id })
            .await
            .expect("session view after clear")
            .snapshot
            .value
            .work_plan
            .is_none()
    );
}

#[tokio::test]
async fn completing_every_todo_clears_the_current_work_plan_automatically() {
    let completed_update = AssistantMessage {
        id: MessageId::new("assistant-complete-plan").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("work-plan-complete-call").expect("tool call id"),
            name: ToolName::new("update_plan").expect("tool name"),
            arguments: json!({
                "objective": "finish the release",
                "items": [{
                    "text": "verify release",
                    "status": "completed"
                }]
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&completed_update)),
            ModelScript::Events(message_events(&assistant_text(
                "assistant-completed-plan-final",
                "release verified",
            ))),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(
        model,
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
    )
    .await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let mut events = runtime.subscribe_events();
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "finish everything".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("submit input");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert!(
        store
            .load_work_plan(&session_id)
            .await
            .expect("load work plan")
            .is_none()
    );
    assert!(
        runtime
            .get_session_view(assistant_protocol::GetSessionViewRequest {
                session_id: session_id.clone(),
            })
            .await
            .expect("session view")
            .snapshot
            .value
            .work_plan
            .is_none()
    );
    let revision = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let assistant_protocol::RuntimeEvent::WorkPlanChanged {
                session_id: changed_session,
                revision,
            } = events.recv().await.expect("work plan event")
                && changed_session == session_id
            {
                break revision;
            }
        }
    })
    .await
    .expect("work plan changed event");
    assert_eq!(revision, 1);
}

#[tokio::test]
async fn recovered_work_plan_is_available_to_the_first_claim_after_restart() {
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let first_runtime = runtime_with_store(
        empty_model(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
    )
    .await;
    let session_id = first_runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    store
        .mutate_work_plan(crate::WorkPlanMutation {
            session_id: session_id.clone(),
            expected_revision: 0,
            operation_id: "recovered-work-plan-call".to_owned(),
            objective: "resume after restart".to_owned(),
            items: vec![crate::StoredWorkPlanItem {
                id: assistant_protocol::TodoItemId::new("todo-recovered").expect("todo item id"),
                text: "continue safely".to_owned(),
                status: crate::StoredTodoItemStatus::Pending,
            }],
            updated_at_ms: 2_000,
        })
        .await
        .expect("seed durable plan");
    drop(first_runtime);

    let model = Arc::new(ScriptedModelService::completing(
        model_capabilities(true),
        8_192,
        assistant_text("assistant-after-restart", "resumed"),
    ));
    let runtime = runtime_with_store(
        model.clone(),
        store,
        RuntimeConfig::new(NonZeroUsize::new(32).expect("non-zero")),
    )
    .await;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "continue".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("submit after restart");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let request = model
        .take_requests()
        .into_iter()
        .next()
        .expect("captured request");
    assert!(request.conversation.messages.iter().any(|message| {
        matches!(message, ConversationMessage::User(user) if user.parts.iter().any(|part| {
            matches!(part, UserPart::Injected(text)
                if text.text.starts_with(crate::work_plan::WORK_PLAN_CONTEXT_V1)
                    && text.text.contains("resume after restart"))
        }))
    }));
}
