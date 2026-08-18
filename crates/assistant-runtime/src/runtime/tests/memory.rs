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
        .system_prompt()
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
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "remember this preference".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
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
