use super::*;
use crate::{
    SkillActivationTrigger, SkillCandidate, SkillMetadata, SkillName, SkillNameStateChange,
    SkillPackageSource, SkillScanFuture, SkillScanRequest, SkillScanResult, SkillSource,
};

pub(super) struct StaticSkillPackageSource;

impl SkillPackageSource for StaticSkillPackageSource {
    fn scan(&self, _request: SkillScanRequest) -> SkillScanFuture<'_> {
        Box::pin(async {
            Ok(SkillScanResult {
                candidates: vec![SkillCandidate {
                    name: SkillName::parse("review").expect("name"),
                    description: "Review changes".to_owned(),
                    source: SkillSource::UserAgents,
                    workspace_root_order: None,
                    source_path: "/fixture/review".to_owned(),
                    definition_digest: format!("sha256-v1:{}", "1".repeat(64)),
                    body: "Review carefully.".to_owned(),
                    metadata: SkillMetadata::default(),
                    model_invocable: true,
                    user_invocable: true,
                }],
                diagnostics: Vec::new(),
                complete: true,
            })
        })
    }
}

#[tokio::test]
async fn config_reload_ensures_one_controller_and_role_restricts_lifecycle_operations() {
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG.to_owned()));
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        source,
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );

    runtime
        .reload_config(assistant_protocol::ReloadConfigRequest::default())
        .await
        .expect("reload creates controller");
    runtime
        .reload_config(assistant_protocol::ReloadConfigRequest::default())
        .await
        .expect("second reload reuses controller");
    let application = runtime
        .get_application_snapshot(Default::default())
        .await
        .expect("application snapshot")
        .snapshot
        .value;
    assert_eq!(application.active_sessions.len(), 1);
    let controller = &application.active_sessions[0];
    assert_eq!(controller.title, "主控会话");
    assert_eq!(
        controller.role,
        assistant_protocol::SessionRoleSnapshot::Controller
    );
    assert_eq!(controller.proxy, None);
    assert_eq!(
        application.controller_availability,
        assistant_protocol::ControllerAvailabilitySnapshot::Available {
            session_id: controller.session_id.clone(),
        }
    );
    assert_eq!(application.additional_controller_count, 0);

    let archive = runtime
        .archive_session(assistant_protocol::ArchiveSessionRequest {
            session_id: controller.session_id.clone(),
        })
        .await
        .expect_err("controller cannot be archived");
    assert_eq!(
        archive.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::SessionRoleRestricted
    );
    let deletion = runtime
        .prepare_delete_session(assistant_protocol::PrepareDeleteSessionRequest {
            session_id: controller.session_id.clone(),
        })
        .await
        .expect_err("controller cannot be deleted");
    assert_eq!(
        deletion.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::SessionRoleRestricted
    );
    let fork = runtime
        .fork_session(assistant_protocol::ForkSessionRequest {
            session_id: controller.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("missing").expect("message id"),
            expected_generation: 1,
        })
        .await
        .expect_err("controller cannot be forked");
    assert_eq!(
        fork.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::SessionRoleRestricted
    );

    runtime
        .create_session_inner(
            assistant_protocol::CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "Secondary Controller",
        )
        .await
        .expect("storage permits another controller");
    let application = runtime
        .get_application_snapshot(Default::default())
        .await
        .expect("multi-controller snapshot")
        .snapshot
        .value;
    let mut controllers = application
        .active_sessions
        .iter()
        .filter(|session| session.role == assistant_protocol::SessionRoleSnapshot::Controller)
        .collect::<Vec<_>>();
    controllers.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    assert_eq!(controllers.len(), 2);
    assert_eq!(application.additional_controller_count, 1);
    assert_eq!(
        application.controller_availability,
        assistant_protocol::ControllerAvailabilitySnapshot::Available {
            session_id: controllers[0].session_id.clone(),
        }
    );
}

#[tokio::test]
async fn missing_configuration_keeps_controller_unavailable_without_a_placeholder_session() {
    let runtime = AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        Arc::new(MissingConfigSource),
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolSetSnapshot::default()),
        Arc::new(TestChildWorkspaceFactory::default()),
    );
    runtime
        .reload_config(assistant_protocol::ReloadConfigRequest::default())
        .await
        .expect("missing configuration is a diagnostic result");
    let application = runtime
        .get_application_snapshot(Default::default())
        .await
        .expect("application snapshot")
        .snapshot
        .value;
    assert!(application.active_sessions.is_empty());
    assert_eq!(
        application.controller_availability,
        assistant_protocol::ControllerAvailabilitySnapshot::Unavailable
    );
    assert_eq!(application.additional_controller_count, 0);
}

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
async fn clear_session_rebuilds_context_and_replaces_the_in_memory_generation() {
    let system_prompt_factory = Arc::new(CountingSystemPromptFactory::new());
    let runtime = runtime_with_factories(
        Arc::new(StaticModelFactory::new(empty_model())),
        system_prompt_factory.clone(),
        ToolSetSnapshot::default(),
        32,
    );
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create clear session");
    let original_prompt = runtime
        .session_for_test(&created.session.session_id)
        .system_prompt()
        .clone();

    let request = assistant_protocol::ClearSessionRequest {
        session_id: created.session.session_id.clone(),
        operation_id: assistant_protocol::IdempotencyKey::new("runtime-clear-1")
            .expect("operation id"),
        expected_generation: 1,
    };
    let cleared = runtime
        .clear_session(request.clone())
        .await
        .expect("clear session");

    assert_eq!(system_prompt_factory.created(), 2);
    assert_eq!(cleared.source_generation, 1);
    assert_eq!(cleared.result_generation, 2);
    assert_eq!(
        cleared.cleanup_status,
        assistant_protocol::SessionHistoryCleanupStatus::Completed
    );
    assert_eq!(cleared.session.message_count, 0);
    assert_eq!(cleared.session.title, "New Session");
    let replacement = runtime.session_for_test(&created.session.session_id);
    assert_ne!(replacement.system_prompt(), &original_prompt);
    assert_eq!(
        replacement
            .system_prompt()
            .parts()
            .last()
            .map(String::as_str),
        Some("Session prompt 2")
    );
    assert!(
        runtime
            .conversation_snapshot(&created.session.session_id)
            .await
            .expect("cleared conversation")
            .messages
            .is_empty()
    );
    assert_eq!(
        runtime
            .get_session_view(assistant_protocol::GetSessionViewRequest {
                session_id: created.session.session_id.clone(),
            })
            .await
            .expect("cleared session view")
            .snapshot
            .value
            .conversation
            .generation,
        2
    );

    let replay = runtime
        .clear_session(request)
        .await
        .expect("replay clear session");
    assert_eq!(replay, cleared);
    let stale = runtime
        .clear_session(assistant_protocol::ClearSessionRequest {
            session_id: created.session.session_id,
            operation_id: assistant_protocol::IdempotencyKey::new("runtime-clear-2")
                .expect("operation id"),
            expected_generation: 1,
        })
        .await
        .expect_err("new operation cannot reuse an old generation");
    assert_eq!(
        stale.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::SnapshotStale
    );
}

#[tokio::test]
async fn controller_session_can_be_cleared_without_changing_its_role() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "主控会话",
        )
        .await
        .expect("create controller");
    let cleared = runtime
        .clear_session(assistant_protocol::ClearSessionRequest {
            session_id: created.session.session_id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("controller-clear")
                .expect("operation id"),
            expected_generation: 1,
        })
        .await
        .expect("clear controller");
    assert_eq!(cleared.result_generation, 2);
    assert_eq!(cleared.session.title, "主控会话");
    assert_eq!(
        cleared.session.role,
        assistant_protocol::SessionRoleSnapshot::Controller
    );
    assert_eq!(runtime.controller_sessions().expect("controllers").len(), 1);
}

#[tokio::test]
async fn clear_context_preparation_failure_preserves_the_existing_generation() {
    let runtime = runtime_with_factories(
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(FailingClearSystemPromptFactory::new()),
        ToolSetSnapshot::default(),
        32,
    );
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create clear preparation fixture");
    let original_prompt = runtime
        .session_for_test(&created.session.session_id)
        .system_prompt()
        .clone();
    let error = runtime
        .clear_session(assistant_protocol::ClearSessionRequest {
            session_id: created.session.session_id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("failing-clear")
                .expect("operation id"),
            expected_generation: 1,
        })
        .await
        .expect_err("clear preparation must fail");
    assert_eq!(
        error.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::AgentBuildFailed
    );
    assert_eq!(
        runtime
            .get_session_view(assistant_protocol::GetSessionViewRequest {
                session_id: created.session.session_id.clone(),
            })
            .await
            .expect("unchanged session view")
            .snapshot
            .value
            .conversation
            .generation,
        1
    );
    assert_eq!(
        runtime
            .session_for_test(&created.session.session_id)
            .system_prompt(),
        &original_prompt
    );
}

#[tokio::test]
async fn skill_management_detail_reads_only_the_selected_current_body() {
    let mut runtime = runtime(empty_model());
    runtime.skill_package_source = Arc::new(StaticSkillPackageSource);

    let result = runtime
        .get_skill_detail(assistant_protocol::GetSkillDetailRequest {
            workspace_id: None,
            name: "review".to_owned(),
        })
        .await
        .expect("skill detail");
    let detail = result.detail.expect("current detail");

    assert_eq!(detail.skill.name, "review");
    assert_eq!(detail.skill.description, "Review changes");
    assert_eq!(detail.body.as_deref(), Some("Review carefully."));
    assert!(detail.diagnostics.is_empty());
}

#[tokio::test]
async fn skill_catalog_is_frozen_per_session_and_name_switch_only_affects_new_sessions() {
    let mut runtime = runtime(empty_model());
    runtime.skill_package_source = Arc::new(StaticSkillPackageSource);
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first session");
    let first_controller = runtime.session_for_test(&first.session.session_id);
    assert_eq!(
        first_controller.skill_catalog().definitions[0]
            .name
            .as_str(),
        "review"
    );
    assert!(
        first_controller
            .system_prompt()
            .parts()
            .iter()
            .any(|part| part.contains("SKILL_CATALOG_V1") && part.contains("name=\"review\""))
    );

    runtime
        .store
        .set_skill_enabled(SkillNameStateChange {
            name: SkillName::parse("review").expect("name"),
            enabled: false,
            updated_at_ms: 10,
        })
        .await
        .expect("disable skill");
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second session");
    assert!(
        runtime
            .session_for_test(&second.session.session_id)
            .skill_catalog()
            .definitions
            .is_empty()
    );
    assert_eq!(
        runtime
            .session_for_test(&first.session.session_id)
            .skill_catalog()
            .definitions
            .len(),
        1
    );
}

#[tokio::test]
async fn user_skill_activation_is_frozen_with_queue_and_conversation_projections() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "skill-answer",
            "done",
        )))],
    ));
    let mut runtime = runtime(model);
    runtime.skill_package_source = Arc::new(StaticSkillPackageSource);
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let controller = runtime.session_for_test(&session_id);
    controller.lock_state().expect("state").queue_paused_by_user = true;

    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "review this change".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: Some("review".to_owned()),
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit skill input");
    let queue =
        crate::runtime::product::queue_snapshot(&controller, &Default::default()).expect("queue");
    assert_eq!(
        queue.items[0]
            .as_message()
            .expect("message")
            .skill
            .as_ref()
            .map(|tag| tag.name.as_str()),
        Some("review")
    );
    {
        let state = controller.lock_state().expect("state");
        let input = state.inputs.get(&submitted.input_id).expect("input");
        let message = input
            .stored
            .queued_message
            .as_ref()
            .expect("queued message");
        assert!(message.parts.iter().any(|part| {
            matches!(part, UserPart::InternalContext(part)
                if part.kind == "skill_activation"
                    && part.text.contains("Review carefully."))
        }));
        assert_eq!(state.skill_activations.len(), 1);
    }

    let disabled = runtime
        .set_skill_enabled(assistant_protocol::SetSkillEnabledRequest {
            workspace_id: None,
            name: "review".to_owned(),
            enabled: false,
        })
        .await
        .expect("disable current discovery");
    assert_eq!(
        disabled.snapshot.skills[0].health,
        assistant_protocol::SkillHealthSnapshot::Disabled
    );
    assert_eq!(
        crate::runtime::product::queue_snapshot(&controller, &Default::default())
            .expect("queue")
            .items[0]
            .as_message()
            .expect("message")
            .skill
            .as_ref()
            .map(|tag| tag.name.as_str()),
        Some("review")
    );

    let revision = controller.lock_state().expect("state").queue_revision;
    runtime
        .resume_queued_input(assistant_protocol::ResumeQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: None,
            expected_revision: revision,
        })
        .await
        .expect("resume queue");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value;
    assert!(matches!(
        &view.conversation.items[0],
        assistant_protocol::ConversationItem::User(user)
            if user.skill.as_ref().is_some_and(|tag| tag.name == "review")
    ));
    assert_eq!(view.active_skills.len(), 1);
    assert_eq!(view.active_skills[0].tag.name, "review");
    assert_eq!(view.skill_catalog.skills[0].name, "review");
    let forked = runtime
        .fork_session(assistant_protocol::ForkSessionRequest {
            session_id,
            fork_point: assistant_protocol::MessageId::new("skill-answer").expect("message id"),
            expected_generation: view.conversation.generation,
        })
        .await
        .expect("fork activated prefix");
    let fork_view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: forked.session.session_id,
        })
        .await
        .expect("fork view")
        .snapshot
        .value;
    assert_eq!(fork_view.active_skills.len(), 1);
    assert_eq!(fork_view.active_skills[0].tag.name, "review");
    assert!(matches!(
        &fork_view.conversation.items[0],
        assistant_protocol::ConversationItem::User(user)
            if user.skill.as_ref().is_some_and(|tag| tag.name == "review")
    ));
}

#[tokio::test]
async fn model_load_skill_commits_hidden_activation_and_continues_at_the_next_run_step() {
    let tool_turn = AssistantMessage {
        id: MessageId::new("model-skill-tool-turn").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("model-skill-call").expect("call id"),
            name: ToolName::new("load_skill").expect("tool name"),
            arguments: json!({"name": "review"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let final_message = assistant_text("model-skill-final", "reviewed");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&tool_turn)),
            ModelScript::Events(message_events(&final_message)),
        ],
    ));
    let mut runtime = runtime(model.clone());
    runtime.skill_package_source = Arc::new(StaticSkillPackageSource);
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "review with a skill".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    let conversation = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("conversation");
    let activation_message = conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(message)
                if message.origin == agent_types::UserMessageOrigin::Runtime =>
            {
                Some(message)
            }
            _ => None,
        })
        .expect("hidden activation message");
    assert_eq!(
        activation_message.transcript_visibility,
        agent_types::TranscriptVisibility::Hidden
    );
    assert!(activation_message.parts.iter().any(|part| {
        matches!(
            part,
            UserPart::InternalContext(part)
                if part.kind == "skill_activation"
                    && part.text.contains("trigger: model")
                    && part.text.contains("Review carefully.")
        )
    }));
    assert!(matches!(
        conversation.messages.last(),
        Some(ConversationMessage::Assistant(message)) if message.id == final_message.id
    ));
    let controller = runtime.session_for_test(&session_id);
    let state = controller.lock_state().expect("state");
    assert_eq!(state.skill_activations.len(), 1);
    assert_eq!(
        state.skill_activations[0].trigger,
        SkillActivationTrigger::Model
    );
    let run = state.runs.get(&submitted.run.run_id).expect("run");
    assert_eq!(run.message_step(&activation_message.id), Some(1));
    assert_eq!(run.message_step(&final_message.id), Some(2));
    drop(state);

    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].conversation.messages.iter().any(|message| {
        matches!(message, ConversationMessage::User(message) if message.id == activation_message.id)
    }));
}

#[tokio::test]
async fn goal_and_skill_keep_objective_clean_and_internal_boundaries_ordered() {
    let mut runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [],
    )));
    runtime.skill_package_source = Arc::new(StaticSkillPackageSource);
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let controller = runtime.session_for_test(&session_id);
    controller.lock_state().expect("state").queue_paused_by_user = true;
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::StartGoal,
            session_id,
            message: "ship release".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: Some("review".to_owned()),
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit Goal with skill");
    let state = controller.lock_state().expect("state");
    let goal = state.goal.as_ref().expect("Goal");
    assert_eq!(goal.objective.payload.len(), 1);
    let message = state
        .inputs
        .get(&submitted.input_id)
        .and_then(|input| input.stored.queued_message.as_ref())
        .expect("queued message");
    let kinds = message
        .parts
        .iter()
        .filter_map(|part| match part {
            UserPart::InternalContext(part) => Some(part.kind.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec!["agent_variant", "goal_start", "skill_activation"]
    );
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
    let mut expected = vec![first.session.clone(), second.session.clone()];
    expected.sort_by(|left, right| {
        right
            .is_pinned
            .cmp(&left.is_pinned)
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let expected_ids = expected
        .into_iter()
        .map(|session| session.session_id)
        .collect::<Vec<_>>();
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
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session.session_id.clone(),
            message: "must not commit".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
