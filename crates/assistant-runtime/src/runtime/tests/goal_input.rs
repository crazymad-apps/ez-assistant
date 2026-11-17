use super::*;

use agent_types::TokenUsage;

use assistant_protocol::{
    ClearGoalRequest, ForkSessionRequest, IdempotencyKey, ReenterFromUserMessageRequest,
    ResumeGoalRequest, StopGoalRequest, SubmitInputMode, SubmitInputRequest,
};

use crate::{RuntimeError, StagedAttachmentUpload, goal::GoalState};

fn goal_identity(
    runtime: &AssistantRuntime,
    session_id: &assistant_protocol::SessionId,
) -> (assistant_protocol::GoalId, u64) {
    let controller = runtime
        .session_loader
        .cached(session_id)
        .expect("registry")
        .expect("session");
    let state = controller.lock_state().expect("state");
    let goal = state.goal.as_ref().expect("Goal");
    (goal.id.clone(), goal.generation)
}

#[tokio::test]
async fn start_goal_is_idempotent_freezes_objective_and_clears_after_completion() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "goal-first-answer",
                "first Goal Run finished",
            ))),
            ModelScript::Events(message_events(&AssistantMessage {
                purpose: agent_types::AssistantMessagePurpose::ModelResponse,
                id: MessageId::new("goal-complete-signal").expect("message id"),
                model: ModelIdentity::new(
                    ProviderId::new("fixture").expect("provider id"),
                    "fixture-model",
                ),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: ToolCallId::new("goal-complete-call").expect("call id"),
                    name: ToolName::new("update_goal").expect("tool name"),
                    arguments: json!({"status": "complete", "summary": "release shipped"}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            })),
            ModelScript::Events(message_events(&assistant_text(
                "goal-final-answer",
                "Goal completed",
            ))),
        ],
    ));
    let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let attachment = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: session_id.clone(),
            original_name: "goal-reference.md".to_owned(),
            staging_path: "/volatile/goal-reference.part".to_owned(),
            blob_hash: "e".repeat(64),
            size_bytes: 12,
            media_type: Some("text/markdown".to_owned()),
        })
        .await
        .expect("upload Goal attachment")
        .attachment;
    let mut events = runtime.subscribe_events();
    let key = IdempotencyKey::new("start-goal-1").expect("key");
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "ship the complete release".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: vec![attachment.attachment_id],
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: Some(key.clone()),
        })
        .await
        .expect("start Goal");
    let duplicate = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "this duplicate payload must be ignored".to_owned(),
            variant: assistant_protocol::AgentVariant::Plan,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: Some(key),
        })
        .await
        .expect("idempotent Goal retry");
    assert_eq!(duplicate.input_id, first.input_id);
    assert_eq!(duplicate.run.run_id, first.run.run_id);
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &first.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let lifecycle = tokio::time::timeout(Duration::from_secs(1), async {
        let mut observed = Vec::new();
        loop {
            let event = events.recv().await.expect("Goal lifecycle event");
            let finished = matches!(
                &event,
                RuntimeEvent::RunFinished { run_id, .. } if run_id == &first.run.run_id
            );
            observed.push(event);
            if finished {
                return observed;
            }
        }
    })
    .await
    .expect("Goal Run terminal event");
    for expected in ["accepted", "started", "finished"] {
        let count = lifecycle
            .iter()
            .filter(|event| match (expected, event) {
                ("accepted", RuntimeEvent::RunAccepted { run_id, .. })
                | ("started", RuntimeEvent::RunStarted { run_id, .. })
                | ("finished", RuntimeEvent::RunFinished { run_id, .. }) => {
                    run_id == &first.run.run_id
                }
                _ => false,
            })
            .count();
        assert_eq!(count, 1, "Goal Run must emit one {expected} event");
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let cleared = runtime
                .session(&session_id)
                .await
                .expect("session controller")
                .lock_state()
                .expect("session state")
                .goal
                .as_ref()
                .is_none();
            if cleared {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Goal auto continuation completes");

    let controller = runtime
        .session(&session_id)
        .await
        .expect("session controller");
    {
        let state = controller.lock_state().expect("session state");
        assert!(state.goal.is_none());
        let input = state.inputs.get(&first.input_id).expect("Goal input");
        assert_eq!(input.stored.origin, crate::InputOrigin::User);
        assert_eq!(
            input
                .stored
                .goal_binding
                .as_ref()
                .expect("Goal binding")
                .generation,
            1
        );
        assert!(state.goal_inputs.is_empty(), "first Goal input was claimed");
        assert!(state.queue_item_ids.is_empty());
    }

    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    let request = requests.first().expect("captured Goal request");
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name.as_str() == "update_goal")
    );
    let goal_user = request
        .conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => Some(user),
            _ => None,
        })
        .expect("Goal user message");
    assert_eq!(goal_user.origin, agent_types::UserMessageOrigin::User);
    assert_eq!(
        goal_user.transcript_visibility,
        agent_types::TranscriptVisibility::Visible
    );
    assert!(goal_user.parts.iter().any(|part| {
        matches!(part, UserPart::InternalContext(text)
            if text.text.starts_with("GOAL_START_INJECTION_V1"))
    }));
    assert!(requests[1].conversation.messages.iter().any(|message| {
        matches!(message, ConversationMessage::User(user)
            if user.origin == agent_types::UserMessageOrigin::Runtime
                && user.transcript_visibility == agent_types::TranscriptVisibility::Hidden
                && user.parts.iter().any(|part| matches!(part, UserPart::InternalContext(text)
                    if text.text.starts_with("GOAL_CONTINUATION_V1"))))
    }));
    let view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("Goal session view")
        .snapshot
        .value;
    assert!(view.composer_capabilities.goal_supported);
    assert!(view.work_plan.is_none(), "Goal does not create a WorkPlan");
    assert!(
        view.goal.is_none(),
        "completed Goal is cleared automatically"
    );
    assert!(view.queue.items.is_empty());
}

#[tokio::test]
async fn start_goal_rejects_a_model_without_tool_calls() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let mut parameters = model_fixture::parameters();
    parameters.tool_calls = assistant_protocol::ModelFeatureSupport::Unsupported;
    parameters.tool_choice = assistant_protocol::ModelToolChoiceSupport::default();
    model_fixture::save_fixed(&runtime, "fixture", parameters).await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    assert!(matches!(
        runtime
            .submit_started_input(SubmitInputRequest {
                delivery: Default::default(),
                mode: SubmitInputMode::StartGoal,
                session_id,
                message: "unsupported Goal".to_owned(),
                variant: assistant_protocol::AgentVariant::Build,
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::GoalUnsupportedByModel { .. })
    ));
}

#[tokio::test]
async fn intervention_resumes_same_running_goal_and_keeps_ordinary_tail_held() {
    use assistant_protocol::{IdempotencyKey, InputDelivery};

    let entered = Arc::new(Notify::new());
    let runtime = runtime_with_tools(
        Arc::new(CancellationAwareModel {
            capabilities: model_capabilities(true),
            entered: entered.clone(),
        }),
        ToolSetSnapshot::default(),
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let mut request = SubmitInputRequest {
        delivery: InputDelivery::Queued,
        mode: SubmitInputMode::StartGoal,
        session_id: session_id.clone(),
        message: "original goal".into(),
        variant: AgentVariant::Build,
        attachment_ids: vec![],
        quotes: vec![],
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    };
    let old = runtime.submit_started_input(request.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let (goal_id, generation) = goal_identity(&runtime, &session_id);
    request.mode = SubmitInputMode::Normal;
    request.message = "ordinary tail".into();
    let tail = runtime.submit_input(request.clone()).await.unwrap();
    request.delivery = InputDelivery::Intervention;
    request.message = "adjust the next step".into();
    request.idempotency_key = Some(IdempotencyKey::new("goal-guidance").unwrap());
    let result = tokio::time::timeout(Duration::from_secs(3), runtime.submit_input(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.user_message.unwrap().text, "adjust the next step");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &old.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    let view = runtime
        .get_session_view(GetSessionViewRequest { session_id })
        .await
        .unwrap()
        .snapshot
        .value;
    let goal = view.goal.unwrap();
    assert_eq!(goal.goal_id, goal_id);
    assert_eq!(goal.generation, generation + 2);
    assert!(
        matches!(&view.queue.items[..], [assistant_protocol::QueuedSessionItemSnapshot::Message(item)] if item.input_id == tail.input_id && item.held_by_goal)
    );
    assert_eq!(view.active_run.unwrap().run_id, result.run.unwrap().run_id);
}

#[tokio::test]
async fn stop_goal_fences_late_run_settlement_then_clear_removes_only_the_controller() {
    let entered = Arc::new(Notify::new());
    let model = Arc::new(CancellationAwareModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
    });
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let started = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "keep running until stopped".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("start Goal");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("Goal Run entered model");

    let queued = runtime
        .submit_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "Queue this while the Goal is running".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("queue ordinary input");
    assert!(queued.run.is_none());
    let view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value;
    assert!(matches!(
        &view.queue.items[..],
        [assistant_protocol::QueuedSessionItemSnapshot::Message(item)]
            if item.input_id == queued.input_id && item.held_by_goal
    ));
    runtime
        .cancel_queued_input(assistant_protocol::CancelQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: queued.input_id,
        })
        .await
        .expect("cancel the held input");

    assert!(matches!(
        runtime
            .cancel_run(assistant_protocol::CancelRunRequest {
                session_id: session_id.clone(),
                run_id: started.run.run_id.clone(),
            })
            .await,
        Err(RuntimeError::GoalRunRequiresResume { .. })
    ));
    let (goal_id, generation) = goal_identity(&runtime, &session_id);
    assert!(matches!(
        runtime
            .stop_goal(StopGoalRequest {
                session_id: session_id.clone(),
                goal_id: goal_id.clone(),
                expected_generation: generation + 1,
            })
            .await,
        Err(RuntimeError::GoalGenerationConflict { .. })
    ));
    let mut events = runtime.subscribe_events();
    runtime
        .stop_goal(StopGoalRequest {
            session_id: session_id.clone(),
            goal_id: goal_id.clone(),
            expected_generation: generation,
        })
        .await
        .expect("stop Goal");
    let changed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let assistant_protocol::RuntimeEvent::GoalChanged {
                session_id: changed_session,
                goal_id: changed_goal,
                generation: changed_generation,
            } = events.recv().await.expect("Goal event")
            {
                break (changed_session, changed_goal, changed_generation);
            }
        }
    })
    .await
    .expect("Goal changed event");
    assert_eq!(
        changed,
        (session_id.clone(), goal_id.clone(), generation + 1)
    );
    {
        let controller = runtime.session(&session_id).await.expect("session");
        let state = controller.lock_state().expect("state");
        let goal = state.goal.as_ref().expect("Goal");
        assert!(matches!(
            goal.state,
            GoalState::Paused(crate::goal::GoalPauseReason::UserStopped)
        ));
        assert_eq!(goal.generation, 2);
    }
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &started.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    runtime
        .clear_goal(ClearGoalRequest {
            session_id: session_id.clone(),
            goal_id,
            expected_generation: generation + 1,
        })
        .await
        .expect("clear Goal");
    let controller = runtime.session(&session_id).await.expect("session");
    let state = controller.lock_state().expect("state");
    assert!(state.goal.is_none());
    assert!(state.work_plan.is_none());
    assert_eq!(state.message_count, 1, "history is retained");
}

#[tokio::test]
async fn blocked_goal_resumes_with_visible_user_guidance_and_a_new_generation() {
    let blocked = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new("goal-blocked-signal").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("goal-blocked-call").expect("call id"),
            name: ToolName::new("update_goal").expect("tool name"),
            arguments: json!({"status": "blocked", "summary": "need release channel"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let complete = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new("goal-resumed-complete-signal").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("goal-resumed-complete-call").expect("call id"),
            name: ToolName::new("update_goal").expect("tool name"),
            arguments: json!({"status": "complete", "summary": "release shipped"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&blocked)),
            ModelScript::Events(message_events(&assistant_text(
                "goal-blocked-final",
                "Waiting for guidance",
            ))),
            ModelScript::Events(message_events(&complete)),
            ModelScript::Events(message_events(&assistant_text(
                "goal-resumed-final",
                "Shipped",
            ))),
        ],
    ));
    let factory = Arc::new(FailOnceModelFactory::new(model.clone()));
    factory.should_fail.store(false, Ordering::SeqCst);
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        32,
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "ship release".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("start Goal");
    wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    let paused_identity = goal_identity(&runtime, &session_id);
    {
        let controller = runtime.session(&session_id).await.expect("session");
        let state = controller.lock_state().expect("state");
        let goal = state.goal.as_ref().expect("Goal");
        assert!(matches!(
            goal.state,
            GoalState::Paused(crate::goal::GoalPauseReason::Blocked { .. })
        ));
        assert_eq!(goal.generation, 2);
    }
    let guidance = SubmitInputRequest {
        delivery: Default::default(),
        mode: SubmitInputMode::Normal,
        session_id: session_id.clone(),
        message: "Use the stable release channel".to_owned(),
        variant: assistant_protocol::AgentVariant::Build,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    };
    factory.should_fail.store(true, Ordering::SeqCst);
    let failed = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            message: "This input fails during preparation".to_owned(),
            ..guidance.clone()
        })
        .await
        .expect("accept input");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &failed.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    assert_eq!(goal_identity(&runtime, &session_id), paused_identity);
    {
        let controller = runtime.session(&session_id).await.expect("session");
        controller.lock_state().expect("state").queue_paused_by_user = true;
    }
    let submitted = runtime
        .submit_input(guidance)
        .await
        .expect("queue guidance");
    assert!(submitted.run.is_none());
    assert_eq!(goal_identity(&runtime, &session_id), paused_identity);
    runtime
        .resume_session(assistant_protocol::ResumeSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("resume the queue");
    let resumed = runtime
        .wait_for_input_claim(&session_id, &submitted.input_id)
        .await
        .expect("claim guidance");
    let finished = wait_for_terminal(&runtime, &session_id, &resumed.run_id).await;
    assert_eq!(finished.status, assistant_protocol::RunStatus::Completed);
    assert!(finished.started_at_ms.expect("started") >= finished.created_at_ms.expect("claimed"));
    {
        let controller = runtime.session(&session_id).await.expect("session");
        let state = controller.lock_state().expect("state");
        assert!(state.goal.is_none());
    }
    let requests = model.take_requests();
    assert!(requests[2].conversation.messages.iter().any(|message| {
        matches!(message, ConversationMessage::User(user)
            if user.origin == agent_types::UserMessageOrigin::User
                && user.transcript_visibility == agent_types::TranscriptVisibility::Visible
                && user.parts.iter().any(|part| matches!(part, UserPart::InternalContext(text)
                    if text.text.starts_with("GOAL_RESUME_INJECTION_V1"))))
    }));
    let resume_message_id = runtime
        .session(&session_id)
        .await
        .expect("session")
        .lock_state()
        .expect("state")
        .inputs
        .get(&submitted.input_id)
        .expect("resume input")
        .stored
        .user_message_id
        .clone();
    let rewritten = runtime
        .reenter_from_user_message({
            let (expected_generation, expected_message_count) =
                edit_basis(&runtime, &session_id.clone()).await;
            ReenterFromUserMessageRequest {
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session_id.clone(),
                message_id: assistant_protocol::MessageId::new(resume_message_id.as_str())
                    .expect("message id"),
                message: "replace guidance after objective".to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: assistant_protocol::IdempotencyKey::new(
                    crate::id::generate("edit-test").unwrap(),
                )
                .unwrap(),

                expected_generation,
                expected_message_count,
                quotes: Vec::new(),
                skill_edit: assistant_protocol::UserMessageSkillEdit::Retain,
                mcp_edit: assistant_protocol::UserMessageMcpEdit::Retain,
            }
        })
        .await
        .expect("rewrite after Goal objective");
    assert_eq!(
        rewritten.run.status,
        assistant_protocol::RunStatus::Accepted,
        "history rewrite becomes a normal input after Goal completion"
    );
    let controller = runtime.session(&session_id).await.expect("session");
    let state = controller.lock_state().expect("state");
    assert!(state.goal.is_none());
    assert!(!state.resume_required);
}

#[tokio::test]
async fn fork_copies_goal_only_when_the_objective_message_is_in_the_prefix() {
    let blocked = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new("fork-goal-blocked-signal").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("fork-goal-blocked-call").expect("call id"),
            name: ToolName::new("update_goal").expect("tool name"),
            arguments: json!({"status": "blocked", "summary": "need input"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "assistant-before-goal",
                "baseline",
            ))),
            ModelScript::Events(message_events(&blocked)),
            ModelScript::Events(message_events(&assistant_text(
                "assistant-goal-fork-point",
                "waiting",
            ))),
        ],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    let source = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("source session")
        .session;
    let baseline = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::Normal,
            session_id: source.session_id.clone(),
            message: "baseline turn".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("baseline input");
    wait_for_terminal(&runtime, &source.session_id, &baseline.run.run_id).await;
    let goal_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: source.session_id.clone(),
            message: "forkable Goal".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("start Goal");
    wait_for_terminal(&runtime, &source.session_id, &goal_run.run.run_id).await;
    let generation = runtime
        .session(&source.session_id)
        .await
        .expect("source controller")
        .lock_state()
        .expect("source state")
        .body_generation;
    let before = runtime
        .fork_session(ForkSessionRequest {
            session_id: source.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("assistant-before-goal")
                .expect("fork point"),
            expected_generation: generation,
        })
        .await
        .expect("fork before Goal")
        .session;
    assert!(
        runtime
            .session(&before.session_id)
            .await
            .expect("before controller")
            .lock_state()
            .expect("before state")
            .goal
            .is_none()
    );
    let after = runtime
        .fork_session(ForkSessionRequest {
            session_id: source.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("assistant-goal-fork-point")
                .expect("fork point"),
            expected_generation: generation,
        })
        .await
        .expect("fork with Goal")
        .session;
    let source_goal_id = runtime
        .session(&source.session_id)
        .await
        .expect("source controller")
        .lock_state()
        .expect("source state")
        .goal
        .as_ref()
        .expect("source Goal")
        .id
        .clone();
    let after_controller = runtime
        .session(&after.session_id)
        .await
        .expect("after controller");
    let after_state = after_controller.lock_state().expect("after state");
    let forked = after_state.goal.as_ref().expect("forked Goal");
    assert_ne!(forked.id, source_goal_id);
    assert_eq!(forked.generation, 1);
    assert!(matches!(
        forked.state,
        GoalState::Paused(crate::goal::GoalPauseReason::Forked)
    ));
}

#[tokio::test]
async fn goal_pauses_after_three_consecutive_run_failures_without_retrying_an_attempt() {
    let failure = || ModelError::Provider {
        message: "temporary provider failure".to_owned(),
        status: Some(503),
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::FailEstablishment(failure()),
            ModelScript::FailEstablishment(failure()),
            ModelScript::FailEstablishment(failure()),
        ],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let controller = runtime.session(&session_id).await.expect("session");
    controller.lock_state().expect("state").queue_paused_by_user = true;
    let queued = runtime
        .submit_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "Wait for the Goal".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("queue ordinary input");
    assert!(queued.run.is_none());
    // 让 Goal 首轮优先领取；随后只由失败恢复门禁阻止普通输入。
    controller.lock_state().expect("state").queue_paused_by_user = false;
    runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "retry across Goal Runs".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("start Goal");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let paused = runtime
                .session(&session_id)
                .await
                .expect("session")
                .lock_state()
                .expect("state")
                .goal
                .as_ref()
                .is_some_and(|goal| {
                    matches!(
                        goal.state,
                        GoalState::Paused(crate::goal::GoalPauseReason::ConsecutiveFailures)
                    )
                });
            if paused {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failure budget pauses Goal");
    let state = controller.lock_state().expect("state");
    let goal = state.goal.as_ref().expect("Goal");
    assert_eq!(goal.budget.used_runs, 3);
    assert_eq!(goal.consecutive_failures, 3);
    assert_eq!(goal.generation, 2);
    assert!(state.resume_required);
    assert!(state.next_runnable_input().is_none());
    assert_eq!(state.queue_item_ids.front(), Some(&queued.input_id));
    assert_eq!(state.inputs.len(), 2, "one Goal Input and one queued input");
    assert_eq!(
        state.runs.len(),
        1,
        "automatic Goal progress keeps the original Run"
    );
    assert!(state.runs.values().all(|run| run.attempt() == 1));
}

#[tokio::test]
async fn paused_goal_can_resume_with_a_hidden_runtime_continuation() {
    let signal = |status: &str, message_id: &str, call_id: &str| AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(call_id).expect("call id"),
            name: ToolName::new("update_goal").expect("tool name"),
            arguments: json!({"status": status, "summary": status}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&signal(
                "blocked",
                "hidden-resume-blocked",
                "hidden-resume-blocked-call",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "hidden-resume-blocked-final",
                "waiting",
            ))),
            ModelScript::Events(message_events(&signal(
                "complete",
                "hidden-resume-complete",
                "hidden-resume-complete-call",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "hidden-resume-complete-final",
                "done",
            ))),
        ],
    ));
    let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "resume without guidance".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("start Goal");
    wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    let (goal_id, generation) = goal_identity(&runtime, &session_id);
    let resumed = runtime
        .resume_goal(ResumeGoalRequest {
            session_id: session_id.clone(),
            goal_id,
            expected_generation: generation,
            input_id: None,
        })
        .await
        .expect("resume without message");
    let resumed_input = runtime
        .session(&session_id)
        .await
        .unwrap()
        .lock_state()
        .unwrap()
        .inputs
        .values()
        .find(|input| {
            input
                .stored
                .goal_binding
                .as_ref()
                .is_some_and(|binding| binding.generation == resumed.goal.generation)
        })
        .unwrap()
        .stored
        .input_id
        .clone();
    let resumed_run = runtime
        .wait_for_input_claim(&session_id, &resumed_input)
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &resumed_run.run_id).await;
    let requests = model.take_requests();
    assert!(requests[2].conversation.messages.iter().any(|message| {
        matches!(message, ConversationMessage::User(user)
            if user.origin == agent_types::UserMessageOrigin::Runtime
                && user.transcript_visibility == agent_types::TranscriptVisibility::Hidden
                && user.parts.iter().any(|part| matches!(part, UserPart::InternalContext(text)
                    if text.text.starts_with("GOAL_CONTINUATION_V1"))))
    }));
    let controller = runtime.session(&session_id).await.expect("session");
    let state = controller.lock_state().expect("state");
    assert!(state.goal.is_none());
}

#[tokio::test]
async fn held_user_input_can_be_bound_to_goal_resume_without_duplication() {
    let signal = |status: &str, message_id: &str, call_id: &str| AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(call_id).expect("call id"),
            name: ToolName::new("update_goal").expect("tool name"),
            arguments: json!({"status": status, "summary": status}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&signal(
                "blocked",
                "held-resume-blocked",
                "held-resume-blocked-call",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "held-resume-blocked-final",
                "waiting",
            ))),
            ModelScript::Events(message_events(&signal(
                "blocked",
                "held-resume-blocked-again",
                "held-resume-blocked-again-call",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "held-resume-blocked-again-final",
                "waiting again",
            ))),
            ModelScript::Events(message_events(&signal(
                "complete",
                "held-resume-complete",
                "held-resume-complete-call",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "held-resume-complete-final",
                "done",
            ))),
        ],
    ));
    let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "wait for held guidance".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("start Goal");
    wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    runtime
        .session(&session_id)
        .await
        .unwrap()
        .lock_state()
        .unwrap()
        .queue_paused_by_user = true;
    let held = runtime
        .submit_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "Use this exact held guidance".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("hold guidance");
    let (goal_id, generation) = goal_identity(&runtime, &session_id);
    let resumed = runtime
        .resume_goal(ResumeGoalRequest {
            session_id: session_id.clone(),
            goal_id,
            expected_generation: generation,
            input_id: Some(held.input_id.clone()),
        })
        .await
        .expect("bind held guidance");
    assert!(resumed.run.is_none());
    let resumed_input = runtime
        .session(&session_id)
        .await
        .unwrap()
        .lock_state()
        .unwrap()
        .inputs
        .values()
        .find(|input| {
            input
                .stored
                .goal_binding
                .as_ref()
                .is_some_and(|binding| binding.generation == resumed.goal.generation)
        })
        .unwrap()
        .stored
        .input_id
        .clone();
    let resumed_run = runtime
        .wait_for_input_claim(&session_id, &resumed_input)
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &resumed_run.run_id).await;
    let requests = model.take_requests();
    assert!(requests[2].conversation.messages.iter().any(|message| {
        matches!(message, ConversationMessage::User(user)
            if user.origin == agent_types::UserMessageOrigin::User
                && user.parts.iter().any(|part| matches!(part, UserPart::Text(text)
                    if text.text == "Use this exact held guidance"))
                && user.parts.iter().any(|part| matches!(part, UserPart::InternalContext(text)
                    if text.text.starts_with("GOAL_RESUME_INJECTION_V1"))))
    }));
    let controller = runtime.session(&session_id).await.expect("session");
    let (target, expected_generation, expected_message_count, goal_id, generation) = {
        let state = controller.lock_state().expect("state");
        assert_eq!(state.inputs.len(), 2, "held Input is reused, not copied");
        assert!(state.queue_item_ids.is_empty());
        let goal = state.goal.as_ref().expect("paused Goal");
        assert!(matches!(goal.state, GoalState::Paused(_)));
        (
            state.inputs[&resumed_input].stored.user_message_id.clone(),
            state.body_generation,
            state.message_count,
            goal.id.clone(),
            goal.generation,
        )
    };
    let request = ReenterFromUserMessageRequest {
        session_id: session_id.clone(),
        message_id: assistant_protocol::MessageId::new(target.as_str()).unwrap(),
        message: "Use the corrected guidance".to_owned(),
        variant: assistant_protocol::AgentVariant::Build,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        expected_generation,
        expected_message_count,
        idempotency_key: IdempotencyKey::new("edit-paused-goal").unwrap(),
        skill_edit: assistant_protocol::UserMessageSkillEdit::Retain,
        mcp_edit: assistant_protocol::UserMessageMcpEdit::Retain,
    };
    let edited = runtime
        .reenter_from_user_message(request.clone())
        .await
        .unwrap();
    assert_eq!(
        runtime
            .reenter_from_user_message(request)
            .await
            .unwrap()
            .run
            .run_id,
        edited.run.run_id
    );
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &edited.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let state = controller.lock_state().expect("state");
    let binding = state.inputs[&edited.input_id]
        .stored
        .goal_binding
        .as_ref()
        .unwrap();
    assert_eq!(binding.goal_id, goal_id);
    assert_eq!(binding.generation, generation + 1);
    assert!(state.goal.is_none());
}

#[tokio::test]
async fn reported_usage_does_not_pause_goal_before_the_run_limit() {
    let costly = |suffix: &str| AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new(format!("goal-token-limit-answer-{suffix}")).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::Text(TextPart {
            id: PartId::new(format!("goal-token-limit-text-{suffix}")).expect("part id"),
            text: "large result".to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: Some(TokenUsage {
            input_tokens: 125_000,
            output_tokens: 125_000,
            total_tokens: 250_000,
            cached_input_tokens: None,
            reasoning_tokens: None,
        }),
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        1_000_000,
        (0..crate::goal::DEFAULT_MAX_RUNS)
            .map(|index| ModelScript::Events(message_events(&costly(&index.to_string())))),
    ));
    let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "bounded Goal".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("start Goal");
    wait_for_terminal(&runtime, &session_id, &run.run.run_id).await;
    let controller = runtime.session(&session_id).await.expect("session");
    let state = controller.lock_state().expect("state");
    let goal = state.goal.as_ref().expect("Goal");
    assert!(
        matches!(
            goal.state,
            GoalState::Paused(crate::goal::GoalPauseReason::RunLimitReached)
        ),
        "unexpected Goal state: {goal:?}"
    );
    assert_eq!(goal.budget.used_total_tokens, 5_000_000);
    assert_eq!(goal.budget.used_runs, crate::goal::DEFAULT_MAX_RUNS);
    assert!(goal.budget.usage_complete);
    assert_eq!(goal.generation, 2);
    assert_eq!(state.inputs.len(), 1);
    assert_eq!(state.runs.len(), 1);
    let requests = model.take_requests();
    assert_eq!(requests.len(), crate::goal::DEFAULT_MAX_RUNS as usize);
    for request in requests {
        assert!(
            !serde_json::to_string(&request.conversation)
                .unwrap()
                .contains("remaining_total_tokens")
        );
    }
}
