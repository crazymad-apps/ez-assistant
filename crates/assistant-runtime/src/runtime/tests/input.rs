use super::*;

use assistant_protocol::{
    AgentVariant, ApprovalMode, CancelQueuedInputRequest, ConversationItem, ConversationOwner,
    IdempotencyKey, ListConversationPageRequest, PartId, QuotedTextSnapshot,
    QuotedTextSourceRoleSnapshot, RetryRunRequest, SetSessionApprovalModeRequest,
    SubmitInputRequest,
};

#[tokio::test]
async fn consumed_input_keeps_identity_and_retry_uses_history_without_losing_queue_priority() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8192,
        [],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let config = RuntimeConfig::new(NonZeroUsize::new(256).unwrap());
    let first = runtime_with_store(model.clone(), store.clone(), config).await;
    let owner = first
        .create_session(CreateSessionRequest {
            title: Some("Input control boundary".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .session
        .session_id;
    let at_ms = super::super::now_ms().unwrap();
    let input = |label: &str, key: Option<IdempotencyKey>| crate::NewStoredInput {
        feedback_answer: None,
        agent_shell_target: None,
        input_id: assistant_protocol::InputId::new(format!("input-{label}")).unwrap(),

        session_id: owner.clone(),
        idempotency_key: key,
        agent_variant: AgentVariant::Build,
        origin: crate::InputOrigin::User,
        goal_binding: None,
        cross_session: None,
        channel_source: Some(crate::InputChannelSource::desktop_text()),
        skill_activation: None,
        mcp_selection: None,

        message: agent_types::UserMessage {
            id: MessageId::new(format!("user-{label}")).unwrap(),
            origin: agent_types::UserMessageOrigin::User,
            transcript_visibility: agent_types::TranscriptVisibility::Visible,
            parts: vec![UserPart::Text(TextPart {
                id: agent_types::PartId::new(format!("text-{label}")).unwrap(),
                text: label.into(),
            })],
        },
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: at_ms,
    };
    // 通过 Store 端口构造确定性持久现场，尚未把它交给任何运行中的执行器。
    let queued = input("queued", None);
    let original = input(
        "history",
        Some(IdempotencyKey::new("original-key").unwrap()),
    );
    store.accept_input(queued.clone()).await.unwrap();
    store.accept_input(original.clone()).await.unwrap();
    let original_order = store
        .load_runtime()
        .await
        .unwrap()
        .inputs
        .into_iter()
        .find(|input| input.input_id == original.input_id)
        .unwrap()
        .queue_order;
    store
        .prioritize_queued_input(crate::QueuePriorityChange {
            session_id: owner.clone(),
            input_id: original.input_id.clone(),
        })
        .await
        .unwrap();
    let before = store.load_runtime().await.unwrap();
    let queued_before = before
        .inputs
        .iter()
        .find(|input| input.input_id == queued.input_id)
        .unwrap()
        .clone();
    assert_ne!(
        before
            .inputs
            .iter()
            .find(|input| input.input_id == original.input_id)
            .unwrap()
            .queue_order,
        original_order
    );
    store
        .claim_input(crate::StoredRun {
            shell: None,
            run_id: RunId::new("run-history").unwrap(),
            session_id: owner.clone(),
            input_id: original.input_id.clone(),
            attempt: 1,
            status: assistant_protocol::RunStatus::Accepted,
            agent_variant: original.agent_variant,
            approval_mode: assistant_protocol::ApprovalMode::Ask,
            reasoning_effort: None,
            cancel_requested: false,
            error: None,
            message_ids: Vec::new(),
            message_steps: Default::default(),
            created_at_ms: at_ms,
            started_at_ms: None,
            finished_at_ms: None,
        })
        .await
        .unwrap();
    assert!(
        store
            .cancel_queued_input(&owner, &original.input_id)
            .await
            .is_err()
    );
    assert!(
        store
            .prioritize_queued_input(crate::QueuePriorityChange {
                session_id: owner.clone(),
                input_id: original.input_id.clone()
            })
            .await
            .is_err()
    );
    store
        .commit_user_message(crate::UserMessageCommit {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            operation_id: "consume-prioritized".into(),
            session_id: owner.clone(),
            input_id: original.input_id.clone(),
            run_id: RunId::new("run-history").unwrap().clone(),
            message: Some(original.message.clone()),
            reasoning_effort: None,
            created_at_ms: at_ms,
        })
        .await
        .unwrap();
    store
        .settle_run(crate::StoredRunSettlement {
            operation_id: "first-failure".into(),
            run_id: RunId::new("run-history").unwrap().clone(),
            session_id: owner.clone(),
            status: assistant_protocol::RunStatus::Failed,
            cancel_requested: false,
            error: Some(assistant_protocol::RuntimeErrorInfo {
                code: assistant_protocol::RuntimeErrorCode::InvalidRequest,
                message: "fixture failure".into(),
            }),
            queued_user_message: None,
            messages: vec![ConversationMessage::Assistant(assistant_text(
                "failed-answer",
                "saved partial answer",
            ))],
            message_step: Some(1),
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: at_ms,
        })
        .await
        .unwrap();
    assert!(!store.contains_input_control(&original.input_id).unwrap());
    let loaded = store.load_runtime().await.unwrap();
    let read_only = loaded
        .inputs
        .iter()
        .find(|input| input.input_id == original.input_id)
        .unwrap();
    assert_eq!(
        read_only.queue_order, original_order,
        "temporary priority is not the original acceptance order"
    );
    assert_eq!(
        loaded
            .inputs
            .iter()
            .find(|input| input.input_id == queued.input_id),
        Some(&queued_before)
    );
    assert!(matches!(
        store.accept_input(original.clone()).await.unwrap(),
        crate::AcceptedInput::Historical { .. }
    ));
    assert!(!store.contains_input_control(&original.input_id).unwrap());
    let saved_body = store.load_conversation(&owner).await.unwrap();
    for field in ["input", "message"] {
        let mut collision = input("collision", None);
        match field {
            "input" => collision.input_id = original.input_id.clone(),
            _ => collision.message.id = original.message.id.clone(),
        }
        assert!(
            store.accept_input(collision).await.is_err(),
            "released {field} identity must not be reused"
        );
        assert_eq!(store.load_conversation(&owner).await.unwrap(), saved_body);
    }
    let retry = store
        .create_run_attempt(crate::NewStoredRunAttempt {
            run_id: RunId::new("actual-retry").unwrap(),
            source_run_id: RunId::new("run-history").unwrap().clone(),
            session_id: owner.clone(),
            approval_mode: ApprovalMode::Auto,
            created_at_ms: at_ms,
        })
        .await
        .unwrap();
    assert!(store.contains_input_control(&original.input_id).unwrap());
    assert_eq!(retry.attempt, 2);
    store
        .commit_user_message(crate::UserMessageCommit {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            operation_id: "begin-actual-retry".into(),
            session_id: owner.clone(),
            input_id: original.input_id.clone(),
            run_id: retry.run_id.clone(),
            message: None,
            reasoning_effort: None,
            created_at_ms: at_ms,
        })
        .await
        .unwrap();
    store
        .settle_run(crate::StoredRunSettlement {
            operation_id: "finish-actual-retry".into(),
            run_id: retry.run_id.clone(),
            session_id: owner.clone(),
            status: assistant_protocol::RunStatus::Completed,
            cancel_requested: false,
            error: None,
            queued_user_message: None,
            messages: vec![ConversationMessage::Assistant(assistant_text(
                "retry-answer",
                "retry completed",
            ))],
            message_step: Some(1),
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: at_ms,
        })
        .await
        .unwrap();
    assert!(!store.contains_input_control(&original.input_id).unwrap());
    let duplicate = store.accept_input(original.clone()).await.unwrap();
    assert_eq!(
        duplicate.run_id(),
        Some(&RunId::new("run-history").unwrap()),
        "idempotency retains the original first attempt"
    );
    let body = store.load_conversation(&owner).await.unwrap();
    assert_eq!(
        body.messages
            .iter()
            .filter(|message| matches!(message, ConversationMessage::User(_)))
            .count(),
        1
    );
    assert_eq!(
        store
            .load_runtime()
            .await
            .unwrap()
            .inputs
            .iter()
            .find(|input| input.input_id == queued.input_id),
        Some(&queued_before)
    );
    drop(first);
    let restarted = runtime_with_store(model.clone(), store.clone(), config).await;
    let restored = restarted
        .get_run(GetRunRequest {
            session_id: owner.clone(),
            run_id: retry.run_id,
        })
        .await
        .unwrap();
    assert_eq!(
        restored.run.status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(restored.run.text, "retry completed");
    assert!(!store.contains_input_control(&original.input_id).unwrap());
    assert!(store.contains_input_control(&queued.input_id).unwrap());
    assert!(
        model.take_requests().is_empty(),
        "history and preparation never start queued work"
    );
}

#[tokio::test]
async fn recovered_history_selection_ids_are_scoped_to_their_session_and_owner() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8192,
        [],
    )));
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let mut stored = runtime.store.load_runtime().await.unwrap();
    for id in [&first, &second] {
        stored.mcp_input_selections.push(crate::StoredMcpSelection {
            selection_id: "same-selection".into(),
            session_id: id.clone(),
            input_id: None,
            message_id: MessageId::new("same-message").unwrap(),
            server_key: assistant_protocol::McpServerKey::new("fixture").unwrap(),
            display_name: id.as_str().into(),
            created_at_ms: 1,
        });
        stored.skill_activations.push(crate::StoredSkillActivation {
            activation_id: "same-activation".into(),
            session_id: id.clone(),
            owner: crate::SkillActivationOwner::Session(id.clone()),
            run_id: None,
            input_id: None,
            message_id: MessageId::new("same-message").unwrap(),
            name: crate::SkillName::parse("review").unwrap(),
            catalog_revision: format!("sha256-v1:{}", "a".repeat(64)),
            definition_digest: format!("sha256-v1:{}", "b".repeat(64)),
            trigger: crate::SkillActivationTrigger::User,
            created_at_ms: 1,
        });
    }
    let registries = super::super::recovery::recover_registries(stored.clone()).unwrap();
    for id in [&first, &second] {
        let state = registries.sessions.get(id).unwrap().lock_state().unwrap();
        assert_eq!(state.mcp_selections.len(), 1);
        assert_eq!(&state.mcp_selections[0].session_id, id);
        assert_eq!(state.skill_activations.len(), 1);
        assert_eq!(&state.skill_activations[0].session_id, id);
        assert!(state.inputs.is_empty());
        assert!(state.runs.is_empty());
    }
    let mut duplicate_mcp = stored.clone();
    duplicate_mcp
        .mcp_input_selections
        .push(stored.mcp_input_selections[0].clone());
    assert!(super::super::recovery::recover_registries(duplicate_mcp).is_err());
    stored
        .skill_activations
        .push(stored.skill_activations[0].clone());
    assert!(super::super::recovery::recover_registries(stored).is_err());
}

#[tokio::test]
async fn quoted_text_direct_locator_is_normalized_and_submitted_as_a_structured_user_part() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("a-quote-1", "first"))),
            ModelScript::Events(message_events(&assistant_text("a-quote-2", "second"))),
        ],
    )));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "prefix selected suffix".to_owned(),
            variant: AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("first input");
    wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    let page = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: ConversationOwner::MainSession {
                session_id: session_id.clone(),
            },
            cursor: None,
            limit: 20,
        })
        .await
        .expect("page")
        .snapshot
        .value;
    let source_message_id = page
        .items
        .iter()
        .find_map(|item| match item {
            ConversationItem::User(message) => Some(message.message_id.clone()),
            ConversationItem::Assistant(_)
            | ConversationItem::ControlResult { .. }
            | ConversationItem::ShellSwitchResult { .. }
            | ConversationItem::SkillRefreshResult { .. }
            | ConversationItem::ContextSummary { .. } => None,
        })
        .expect("source user message");
    let quote = QuotedTextSnapshot {
        quote_id: PartId::new("quote-1").expect("quote id"),
        exact: "selected".to_owned(),
        prefix: "prefix ".to_owned(),
        suffix: " suffix".to_owned(),
        source_owner: page.owner.clone(),
        source_generation: page.generation,
        source_message_id: source_message_id.clone(),
        text_start_utf16: 7,
        text_end_utf16: 15,
        source_role: QuotedTextSourceRoleSnapshot::User,
        source_label: "source session".to_owned(),
        source_created_at_ms: None,
        source_available: true,
    };
    let mut stale = quote.clone();
    stale.source_generation += 1;
    let normalized = runtime
        .normalize_quotes(&session_id, &[stale])
        .await
        .expect("stale source should degrade");
    assert!(!normalized[0].source_available);

    let second = runtime
        .submit_started_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: String::new(),
            variant: AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: vec![quote],
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("quoted input");
    wait_for_terminal(&runtime, &session_id, &second.run.run_id).await;
    let conversation = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("conversation");
    let quoted = conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(message) => {
                message.parts.iter().find_map(|part| match part {
                    UserPart::QuotedText(quote) => Some(quote),
                    _ => None,
                })
            }
            _ => None,
        });
    let quoted = quoted.expect("quoted part");
    assert_eq!(quoted.exact, "selected");
    assert!(quoted.source_available);
}

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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: AgentVariant::Plan,
            session_id: session.session.session_id.clone(),
            message: "first payload".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: Some(key.clone()),
        })
        .await
        .expect("first submit");
    let repeated = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "different payload is ignored for the same key".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: Some("INVALID-SKILL-NAME".to_owned()),
            mcp_server_key: None,
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
            .await
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
    assert!(
        user.parts.iter().all(|part| !matches!(
            part,
            UserPart::InternalContext(part) if part.kind == "channel_input"
        )),
        "Desktop input must not receive channel metadata injection"
    );
    let mut stored = runtime.store.load_runtime().await.unwrap();
    assert!(
        stored.runs.is_empty(),
        "completed Runs are file history, not recovery controls"
    );
    assert_eq!(stored.run_history.len(), 1);
    // 即使没有任何旧执行行，首次身份及幂等返回也必须成立。
    stored.runs.clear();
    let mut recovered = super::super::recovery::recover_registries(stored).unwrap();
    let recovered_session = recovered
        .sessions
        .remove(&session.session.session_id)
        .unwrap();
    assert!(recovered_session.lock_state().unwrap().runs.is_empty());
    let (input_id, run) = runtime
        .find_idempotent_run(
            &recovered_session,
            &IdempotencyKey::new("submit-1").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input_id, first.input_id);
    assert_eq!(run.as_ref().unwrap().run_id, first.run.run_id);
    assert_eq!(
        run.as_ref().unwrap().status,
        assistant_protocol::RunStatus::Completed
    );
    assert!(recovered_session.lock_state().unwrap().runs.is_empty());
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "active".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("queued");
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone()
            })
            .await
            .expect("summary")
            .session
            .queued_input_count,
        1
    );
    assert!(queued.run.is_none(), "暂存输入没有 Run");
    let snapshot = runtime
        .store
        .load_session_state(&session.session.session_id)
        .await
        .unwrap();
    assert!(
        !snapshot
            .state
            .runs
            .iter()
            .any(|run| run.input_id == queued.input_id)
    );
    let queued_id = queued.input_id.clone();
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
            .await
            .expect("summary")
            .session
            .queued_input_count,
        0
    );
    assert!(
        !runtime
            .product_run_snapshots(&runtime.session(&session.session.session_id).await.unwrap())
            .await
            .unwrap()
            .iter()
            .any(|run| run.input_id == queued_id)
    );
}

#[tokio::test]
async fn same_queue_item_ids_execute_in_acceptance_order() {
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
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "first".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("second queued");
    assert!(second.run.is_none());
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session.session_id.clone()
            })
            .await
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
    let second_run = runtime
        .wait_for_input_claim(&session.session.session_id, &second.input_id)
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session.session.session_id, &second_run.run_id)
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
                    | UserPart::QuotedText(_)
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
    model_fixture::seed(&runtime, "unique-test-secret-9f1ca2").await;
    runtime
        .reload_config(ReloadConfigRequest::default())
        .await
        .expect("load config");
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "retry me".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
    let failed_conversation = runtime
        .conversation_snapshot(&session.session.session_id)
        .await
        .expect("failed conversation");
    assert_eq!(failed_conversation.messages.len(), 1);
    assert!(matches!(
        &failed_conversation.messages[0],
        ConversationMessage::User(message)
            if message.parts.iter().any(
                |part| matches!(part, UserPart::Text(text) if text.text == "retry me")
            )
    ));
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
            run_id: first.run.run_id.clone(),
        })
        .await
        .expect("retry");
    assert_eq!(retry.run.input_id, first.input_id);
    assert_eq!(retry.run.attempt, 2);
    assert_eq!(retry.run.variant, AgentVariant::Build);
    assert_eq!(retry.run.approval_mode, ApprovalMode::Auto);
    assert!(retry.run.started_at_ms.is_none());
    let completed_retry =
        wait_for_terminal(&runtime, &session.session.session_id, &retry.run.run_id).await;
    assert!(completed_retry.started_at_ms.is_some());
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
    let loaded = runtime
        .store
        .load_session_state(&session.session.session_id)
        .await
        .unwrap();
    assert!(loaded.state.runs.is_empty());
    assert_eq!(loaded.state.run_history.len(), 2);
    let mut recovered = super::super::recovery::recover_session_registries(loaded).unwrap();
    let recovered_session = recovered
        .sessions
        .remove(&session.session.session_id)
        .unwrap();
    let state = recovered_session.lock_state().unwrap();
    assert!(state.runs.is_empty());
    let input = state.inputs.get(&first.input_id).unwrap();
    assert_eq!(input.first_run_id, Some(first.run.run_id));
    assert_eq!(input.latest_run_id, Some(retry.run.run_id));
}

#[tokio::test]
async fn queued_input_recovers_without_run_and_idempotent_resubmit_does_not_claim_it() {
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8192,
        [ModelScript::Events(message_events(&assistant_text(
            "queue-answer",
            "done",
        )))],
    ));
    let first = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(256).unwrap()),
    )
    .await;
    let session_id = first
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let input_id = assistant_protocol::InputId::new("queued-without-run").unwrap();
    let key = IdempotencyKey::new("queued-key").unwrap();
    let input = crate::NewStoredInput {
        feedback_answer: None,
        agent_shell_target: None,
        input_id: input_id.clone(),
        session_id: session_id.clone(),
        idempotency_key: Some(key.clone()),
        agent_variant: AgentVariant::Build,
        origin: crate::InputOrigin::User,
        goal_binding: None,
        cross_session: None,
        channel_source: Some(crate::InputChannelSource::desktop_text()),
        skill_activation: None,
        mcp_selection: None,
        message: crate::run::create_user_message(
            "queued body".into(),
            Vec::new(),
            AgentVariant::Build,
        )
        .unwrap(),
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: super::super::now_ms().unwrap(),
    };
    assert!(matches!(
        store.accept_input(input).await.unwrap(),
        crate::AcceptedInput::Control { run: None, .. }
    ));
    let before = store.load_runtime().await.unwrap();
    assert!(before.runs.is_empty());
    assert!(before.run_history.is_empty());
    assert!(
        store
            .load_conversation(&session_id)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    first
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
    let runtime = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(256).unwrap()),
    )
    .await;
    let summary = runtime
        .get_session(GetSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .unwrap()
        .session;
    assert_eq!(summary.queued_input_count, 1);
    assert!(summary.resume_required);
    assert!(
        runtime
            .list_runs(assistant_protocol::ListRunsRequest {
                session_id: session_id.clone()
            })
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    let duplicate = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "ignored retry body".into(),
            variant: AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: Some(key),
        })
        .await
        .unwrap();
    assert_eq!(duplicate.input_id, input_id);
    assert!(duplicate.run.is_none());
    assert!(model.take_requests().is_empty());
    runtime
        .resume_session(assistant_protocol::ResumeSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();
    let claimed = runtime
        .wait_for_input_claim(&session_id, &input_id)
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &claimed.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let body = store.load_conversation(&session_id).await.unwrap();
    assert_eq!(
        body.messages
            .iter()
            .filter(|message| matches!(message, ConversationMessage::User(_)))
            .count(),
        1
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}
