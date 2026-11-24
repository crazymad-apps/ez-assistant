use std::collections::BTreeMap;

use agent_tools::{PinMemoryTool, ToolRegistry};
use assistant_protocol::{
    ApprovalDecision, CreatePinnedMemoryRequest, CreateSessionRequest, DecideApprovalRequest,
    DeletePinnedMemoryRequest, GetPersonaRequest, GetSystemContextRequest,
    ListPinnedMemoriesRequest, RunStatus, SetPersonaRequest, ToolActivityStatus,
    UpdatePinnedMemoryRequest,
};

use super::*;
use crate::pinned_memory_limits;

struct PinnedMemoryRunToolFactory;

impl RunToolFactory for PinnedMemoryRunToolFactory {
    fn compile(
        &self,
        request: crate::RunToolFactoryRequest<'_>,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        let mut registry = ToolRegistry::new();
        registry
            .register(PinMemoryTool::new(
                request.pinned_memory,
                pinned_memory_limits(),
            ))
            .map_err(|source| {
                RunToolFactoryError::with_source(
                    RunToolFactoryErrorKind::InvalidConfiguration,
                    source,
                )
            })?;
        Ok(RunToolBundle::new(registry.snapshot(), Vec::new()))
    }
}

#[tokio::test]
async fn persona_mutation_uses_revision_cas_and_accepts_an_enabled_empty_value() {
    let runtime = runtime(empty_model());
    let initial = runtime
        .get_persona(GetPersonaRequest {})
        .await
        .expect("initial persona");
    assert_eq!(initial.persona.revision, 0);

    let updated = runtime
        .set_persona(SetPersonaRequest {
            enabled: true,
            content: String::new(),
            expected_revision: initial.persona.revision,
        })
        .await
        .expect("update persona");
    assert!(updated.applied);
    assert!(updated.persona.enabled);
    assert_eq!(updated.persona.revision, 1);

    let conflict = runtime
        .set_persona(SetPersonaRequest {
            enabled: true,
            content: "stale draft".to_owned(),
            expected_revision: 0,
        })
        .await
        .expect("persona conflict is a product result");
    assert!(!conflict.applied);
    assert_eq!(conflict.persona, updated.persona);
}

#[tokio::test]
async fn pinned_memory_crud_returns_current_collection_after_each_mutation() {
    let runtime = runtime(empty_model());

    let created = runtime
        .create_pinned_memory(CreatePinnedMemoryRequest {
            expected_collection_revision: 0,
            category: "collaboration".to_owned(),
            content: "verify before reporting".to_owned(),
            attributes: BTreeMap::new(),
        })
        .await
        .expect("create pinned memory");
    assert!(created.applied);
    assert_eq!(created.collection.revision, 1);
    let memory = created.memory.expect("created memory");

    let stale_create = runtime
        .create_pinned_memory(CreatePinnedMemoryRequest {
            expected_collection_revision: 0,
            category: "stale".to_owned(),
            content: "must not be created".to_owned(),
            attributes: BTreeMap::new(),
        })
        .await
        .expect("collection conflict is a product result");
    assert!(!stale_create.applied);
    assert_eq!(stale_create.collection.items.len(), 1);

    let updated = runtime
        .update_pinned_memory(UpdatePinnedMemoryRequest {
            id: memory.id.clone(),
            expected_revision: memory.revision,
            category: memory.category.clone(),
            content: "verify, then report evidence".to_owned(),
            attributes: BTreeMap::new(),
        })
        .await
        .expect("update pinned memory");
    assert!(updated.applied);
    assert_eq!(updated.collection.revision, 2);
    assert_eq!(updated.collection.items[0].revision, 2);

    let deleted = runtime
        .delete_pinned_memory(DeletePinnedMemoryRequest {
            id: memory.id,
            expected_revision: 2,
        })
        .await
        .expect("delete pinned memory");
    assert!(deleted.applied);
    assert_eq!(deleted.collection.revision, 3);
    assert!(deleted.collection.items.is_empty());
    assert!(
        runtime
            .list_pinned_memories(ListPinnedMemoriesRequest {})
            .await
            .expect("list pinned memories")
            .collection
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn system_context_query_returns_the_frozen_session_prompt_verbatim() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session");
    let session_id = created.session.session_id;
    let expected = runtime
        .session_for_test(&session_id)
        .await
        .current_system_prompt()
        .expect("system prompt")
        .parts()
        .to_vec();

    let result = runtime
        .get_system_context(GetSystemContextRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("get frozen system context");

    assert_eq!(result.snapshot.session_id, session_id);
    assert_eq!(result.snapshot.parts, expected);
}

#[tokio::test]
async fn agent_pinned_memory_tool_uses_approval_reliable_exchange_and_runtime_store() {
    let tool_message = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new("assistant-pin-memory").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("pin-memory-call").expect("tool call id"),
            name: ToolName::new("pin_memory").expect("tool name"),
            arguments: json!({
                "category": "collaboration",
                "content": "verify evidence before reporting"
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&tool_message)),
            ModelScript::Events(message_events(&assistant_text("assistant-final", "saved"))),
        ],
    ));
    let runtime = runtime_with_run_tool_factory(model, Arc::new(PinnedMemoryRunToolFactory));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "remember this preference".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;

    let pending = wait_for_pending_approval(&runtime, &session_id).await;
    assert!(matches!(
        pending.subject,
        assistant_protocol::ToolApprovalSubject::General { ref tool_name }
            if tool_name == "pin_memory"
    ));
    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::AllowOnce,
        })
        .await
        .expect("allow memory mutation");

    let terminal = wait_for_terminal(&runtime, &session_id, &run.run_id).await;
    assert_eq!(terminal.status, RunStatus::Completed);
    assert_eq!(terminal.tools.len(), 1);
    assert_eq!(terminal.tools[0].status, ToolActivityStatus::Completed);

    let collection = runtime
        .list_pinned_memories(ListPinnedMemoriesRequest {})
        .await
        .expect("list pinned memories")
        .collection;
    assert_eq!(collection.items.len(), 1);
    assert_eq!(
        collection.items[0].content,
        "verify evidence before reporting"
    );
    assert!(matches!(
        &collection.items[0].created_by,
        assistant_protocol::PinnedMemoryCreatedBy::AgentTool {
            session_id: creator_session_id
        } if creator_session_id == &session_id
    ));
}

#[tokio::test]
async fn controller_prompt_updates_live_invalidates_server_history_and_isolates_standard_sessions()
{
    use assistant_protocol::{
        GetControllerPromptRequest, SetControllerPromptRequest, SubmitInputRequest,
    };

    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "ctrl-before",
                "controller before prompt",
            ))),
            ModelScript::Events(message_events(&assistant_text("ctrl-after", "简短回答。"))),
            ModelScript::Events(message_events(&assistant_text(
                "std-after",
                "standard response",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "ctrl-disabled",
                "controller after disable",
            ))),
        ],
    ));
    let runtime = runtime(model.clone());
    let controller = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "助理",
        )
        .await
        .expect("create controller")
        .session
        .session_id;
    let standard = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create standard")
        .session
        .session_id;

    let initial = runtime
        .get_controller_prompt(GetControllerPromptRequest {})
        .await
        .expect("initial controller prompt");
    assert!(!initial.prompt.enabled);
    assert!(initial.prompt.content.is_empty());
    assert_eq!(initial.prompt.revision, 0);
    assert_eq!(
        usize::try_from(initial.max_bytes).expect("max bytes fits usize"),
        crate::MAX_CONTROLLER_PROMPT_BYTES
    );

    let first_ctrl = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "first".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("first controller input");
    assert_eq!(
        wait_for_terminal(&runtime, &controller, &first_ctrl.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    {
        let ctrl_handle = runtime.session_for_test(&controller).await;
        ctrl_handle
            .lock_state()
            .expect("lock controller state")
            .server_history_key = Some("stale-server-binding".to_owned());
        let std_handle = runtime.session_for_test(&standard).await;
        std_handle
            .lock_state()
            .expect("lock standard state")
            .server_history_key = Some("std-server-binding".to_owned());
    }

    let updated = runtime
        .set_controller_prompt(SetControllerPromptRequest {
            enabled: true,
            content: "用 1-2 句口语简短回答。".to_owned(),
            expected_revision: 0,
        })
        .await
        .expect("update controller prompt");
    assert!(updated.applied);
    assert!(updated.prompt.enabled);
    assert_eq!(updated.prompt.revision, 1);

    let stale = runtime
        .set_controller_prompt(SetControllerPromptRequest {
            enabled: false,
            content: "stale".to_owned(),
            expected_revision: 0,
        })
        .await
        .expect("stale cas returns product result");
    assert!(!stale.applied);
    assert_eq!(stale.prompt, updated.prompt);

    {
        let ctrl_handle = runtime.session_for_test(&controller).await;
        let state = ctrl_handle.lock_state().expect("lock controller state");
        assert!(state.server_history_key.is_some());
        assert_ne!(
            state.server_history_key.as_deref(),
            Some("stale-server-binding")
        );
    }
    {
        let std_handle = runtime.session_for_test(&standard).await;
        let std_state = std_handle.lock_state().expect("lock standard state");
        assert_eq!(
            std_state.server_history_key.as_deref(),
            Some("std-server-binding")
        );
    }

    let ctrl_context = runtime
        .get_system_context(GetSystemContextRequest {
            session_id: controller.clone(),
        })
        .await
        .expect("controller system context");
    assert!(
        ctrl_context
            .snapshot
            .parts
            .iter()
            .any(|part| part.contains("<controller_instructions>")
                && part.contains("用 1-2 句口语简短回答。"))
    );

    let std_context = runtime
        .get_system_context(GetSystemContextRequest {
            session_id: standard.clone(),
        })
        .await
        .expect("standard system context");
    assert!(
        !std_context
            .snapshot
            .parts
            .iter()
            .any(|part| part.contains("<controller_instructions>")
                || part.contains("用 1-2 句口语简短回答。"))
    );

    let second_ctrl = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "second".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("second controller input");
    assert_eq!(
        wait_for_terminal(&runtime, &controller, &second_ctrl.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    let std_run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: standard.clone(),
            message: "ordinary".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("standard input");
    assert_eq!(
        wait_for_terminal(&runtime, &standard, &std_run.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    let disabled = runtime
        .set_controller_prompt(SetControllerPromptRequest {
            enabled: false,
            content: "用 1-2 句口语简短回答。".to_owned(),
            expected_revision: 1,
        })
        .await
        .expect("disable controller prompt");
    assert!(disabled.applied);
    assert_eq!(disabled.prompt.revision, 2);

    let third_ctrl = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "third".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("third controller input");
    assert_eq!(
        wait_for_terminal(&runtime, &controller, &third_ctrl.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    let requests = model.take_requests();
    assert_eq!(requests.len(), 4);
    assert!(
        !serde_json::to_string(&requests[0])
            .unwrap()
            .contains("<controller_instructions>")
    );
    let second_req = serde_json::to_string(&requests[1]).unwrap();
    assert!(second_req.contains("<controller_instructions>"));
    assert!(second_req.contains("用 1-2 句口语简短回答。"));
    assert!(second_req.contains("first"));
    assert!(
        !serde_json::to_string(&requests[2])
            .unwrap()
            .contains("<controller_instructions>")
    );
    assert!(
        !serde_json::to_string(&requests[3])
            .unwrap()
            .contains("<controller_instructions>")
    );
}

#[tokio::test]
async fn controller_prompt_does_not_leak_into_delegated_child_tasks() {
    use assistant_protocol::{SetControllerPromptRequest, SubmitInputRequest};

    let delegate_call = AssistantMessage {
        purpose: agent_types::AssistantMessagePurpose::ModelResponse,
        id: MessageId::new("ctrl-delegate").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("delegate-call-ctrl").expect("call id"),
            name: ToolName::new("delegate_task").expect("tool name"),
            arguments: json!({
                "title": "Inspect",
                "task": "Return detailed findings.",
                "context": null,
                "expected_output": "Full structured answer."
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&delegate_call)),
            ModelScript::Events(message_events(&assistant_text(
                "child-detail",
                "detailed child output",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "parent-brief",
                "好，已完成。",
            ))),
        ],
    ));
    let runtime = runtime(model.clone());
    let delegation_config = format!(
        "{TEST_CONFIG}\n[agent.defaults.delegation]\nmax_tasks_per_run = 4\nmax_concurrent_tasks = 2\ntask_timeout_ms = 30000\nmax_steps = 10\nmax_tool_calls = 20\nmax_output_tokens = 1024\n"
    );
    runtime
        .config_registry
        .replace_document_for_test(&delegation_config);

    runtime
        .set_controller_prompt(SetControllerPromptRequest {
            enabled: true,
            content: "只用一句话口语回答。".to_owned(),
            expected_revision: 0,
        })
        .await
        .expect("enable controller prompt");

    let controller = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "助理",
        )
        .await
        .expect("create controller")
        .session
        .session_id;
    set_auto_approval(&runtime, &controller).await;

    let run = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: controller.clone(),
            message: "帮我详细调查再汇报".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit controller input")
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &controller, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .system
            .parts()
            .iter()
            .any(|part| part.contains("<controller_instructions>")
                && part.contains("只用一句话口语回答。"))
    );
    assert!(
        requests[1]
            .system
            .parts()
            .iter()
            .any(|part| part.contains("non-recursive child agent"))
    );
    assert!(
        !requests[1]
            .system
            .parts()
            .iter()
            .any(|part| part.contains("<controller_instructions>")
                || part.contains("只用一句话口语回答。"))
    );
}
