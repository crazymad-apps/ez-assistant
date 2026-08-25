use assistant_protocol::{
    ApprovalDecision, ApprovalStatus, CancelRunRequest, CreateSessionRequest,
    DecideApprovalRequest, ListPendingApprovalsRequest, RegisterWorkspaceRequest,
    ReloadPermissionsRequest, RunStatus, RuntimeEvent, ShutdownRuntimeRequest,
};

use super::*;

fn approval_runtime(
    scripts: impl IntoIterator<Item = ModelScript>,
) -> (AssistantRuntime, ScriptedTool) {
    let tool = ScriptedTool::succeed("approval_tool", json!({"approved": true}), OrderLog::new());
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("register tool");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        scripts,
    ));
    (runtime_with_tools(model, registry.snapshot()), tool)
}

fn tool_step(message_id: &str) -> ModelScript {
    ModelScript::Events(message_events(&AssistantMessage {
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(format!("{message_id}-call")).expect("tool call id"),
            name: ToolName::new("approval_tool").expect("tool name"),
            arguments: json!({"value": "hello"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }))
}

fn final_step(message_id: &str) -> ModelScript {
    ModelScript::Events(message_events(&assistant_text(message_id, "done")))
}

#[tokio::test]
async fn allow_once_resumes_exactly_one_waiting_call_and_emits_lifecycle_events() {
    let (runtime, tool) =
        approval_runtime([tool_step("assistant-tool"), final_step("assistant-final")]);
    let mut events = runtime.subscribe_events();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "use the tool".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &session_id).await;
    assert_eq!(pending.status, ApprovalStatus::Pending);
    assert_eq!(
        pending.available_decisions,
        vec![
            ApprovalDecision::AllowOnce,
            ApprovalDecision::AllowSession,
            ApprovalDecision::Deny,
        ]
    );

    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id.clone(),
            decision: ApprovalDecision::AllowOnce,
        })
        .await
        .expect("allow once");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert_eq!(tool.executed_inputs().len(), 1);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session_id.clone(),
            })
            .expect("list approvals")
            .approvals
            .is_empty()
    );
    assert!(matches!(
        runtime
            .decide_approval(DecideApprovalRequest {
                session_id,
                approval_id: pending.approval_id,
                decision: ApprovalDecision::AllowOnce,
            })
            .await,
        Err(RuntimeError::ApprovalNotFound { .. })
    ));
    assert_eq!(tool.executed_inputs().len(), 1);

    let mut requested = false;
    let mut resolved = false;
    while let Ok(event) = events.try_recv() {
        requested |= matches!(event, RuntimeEvent::ApprovalRequested { .. });
        resolved |= matches!(event, RuntimeEvent::ApprovalResolved { .. });
    }
    assert!(requested && resolved);
}

#[tokio::test]
async fn session_allow_is_persisted_before_resume_and_matches_the_next_run() {
    let (runtime, tool) = approval_runtime([
        tool_step("assistant-tool-1"),
        final_step("assistant-final-1"),
        tool_step("assistant-tool-2"),
        final_step("assistant-final-2"),
    ]);
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
            message: "first".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("first input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &session_id).await;
    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::AllowSession,
        })
        .await
        .expect("persist session allow");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &first.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    let rules = runtime
        .permission_coordinator
        .registry()
        .snapshot(&PermissionFileScope::Session(session_id.clone()))
        .expect("permission registry")
        .expect("session permission snapshot")
        .document
        .as_ref()
        .expect("valid permission document")
        .rules
        .clone();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].effect, crate::PermissionEffect::Allow);

    let second = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "second".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("second input")
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &second.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert_eq!(tool.executed_inputs().len(), 2);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest { session_id })
            .expect("list approvals")
            .approvals
            .is_empty()
    );
}

#[tokio::test]
async fn workspace_allow_applies_to_another_session_bound_to_the_same_workspace() {
    let (runtime, tool) = approval_runtime([
        tool_step("assistant-tool-1"),
        final_step("assistant-final-1"),
        tool_step("assistant-tool-2"),
        final_step("assistant-final-2"),
    ]);
    let workspace_id = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/shared-approval".to_owned(),
        })
        .await
        .expect("register workspace")
        .workspace
        .workspace_id;
    let first_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(workspace_id.clone()),
        })
        .await
        .expect("create first session")
        .session
        .session_id;
    let second_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(workspace_id.clone()),
        })
        .await
        .expect("create second session")
        .session
        .session_id;
    let first = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: first_session.clone(),
            message: "first".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("first input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &first_session).await;
    assert!(
        pending
            .available_decisions
            .contains(&ApprovalDecision::AllowWorkspace)
    );
    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: first_session.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::AllowWorkspace,
        })
        .await
        .expect("persist workspace allow");
    assert_eq!(
        wait_for_terminal(&runtime, &first_session, &first.run_id)
            .await
            .status,
        RunStatus::Completed
    );

    let second = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: second_session.clone(),
            message: "second".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("second input")
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &second_session, &second.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert_eq!(tool.executed_inputs().len(), 2);
    assert_eq!(
        runtime
            .permission_coordinator
            .registry()
            .snapshot(&PermissionFileScope::Workspace(workspace_id))
            .expect("permission registry")
            .expect("workspace snapshot")
            .document
            .as_ref()
            .expect("valid permission document")
            .rules
            .len(),
        1
    );
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: second_session,
            })
            .expect("list approvals")
            .approvals
            .is_empty()
    );
}

#[tokio::test]
async fn unavailable_workspace_scope_keeps_the_approval_pending_until_a_valid_decision() {
    let (runtime, tool) =
        approval_runtime([tool_step("assistant-tool"), final_step("assistant-final")]);
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "use the tool".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &session_id).await;
    assert!(
        !pending
            .available_decisions
            .contains(&ApprovalDecision::AllowWorkspace)
    );
    assert!(matches!(
        runtime
            .decide_approval(DecideApprovalRequest {
                session_id: session_id.clone(),
                approval_id: pending.approval_id.clone(),
                decision: ApprovalDecision::AllowWorkspace,
            })
            .await,
        Err(RuntimeError::PermissionScopeUnavailable)
    ));
    assert_eq!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session_id.clone(),
            })
            .expect("list pending")
            .approvals[0]
            .status,
        ApprovalStatus::Pending
    );

    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await
        .expect("deny tool");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert!(tool.executed_inputs().is_empty());
}

#[tokio::test]
async fn permission_write_failure_keeps_the_call_pending_and_never_executes_it() {
    let tool = ScriptedTool::succeed("approval_tool", json!({"approved": true}), OrderLog::new());
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("register tool");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [tool_step("assistant-tool"), final_step("assistant-final")],
    ));
    let runtime = super::permission::runtime_with_permission_components(
        Arc::new(super::permission::MutablePermissionStore::default()),
        model,
        registry.snapshot(),
    )
    .await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "use the tool".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &session_id).await;

    assert!(matches!(
        runtime
            .decide_approval(DecideApprovalRequest {
                session_id: session_id.clone(),
                approval_id: pending.approval_id.clone(),
                decision: ApprovalDecision::AllowSession,
            })
            .await,
        Err(RuntimeError::PermissionPersistenceFailed)
    ));
    assert!(tool.executed_inputs().is_empty());
    assert_eq!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session_id.clone(),
            })
            .expect("list pending")
            .approvals[0]
            .status,
        ApprovalStatus::Pending
    );

    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::Deny,
        })
        .await
        .expect("deny after failed persistence");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert!(tool.executed_inputs().is_empty());
}

#[tokio::test]
async fn one_permission_revision_conflict_is_reloaded_and_retried_before_resume() {
    let tool = ScriptedTool::succeed("approval_tool", json!({"approved": true}), OrderLog::new());
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("register tool");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [tool_step("assistant-tool"), final_step("assistant-final")],
    ));
    let runtime = super::permission::runtime_with_permission_components(
        Arc::new(super::permission::MutablePermissionStore::conflict_once()),
        model,
        registry.snapshot(),
    )
    .await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "use the tool".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &session_id).await;

    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::AllowSession,
        })
        .await
        .expect("one conflict is retried");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert_eq!(tool.executed_inputs().len(), 1);
}

#[tokio::test]
async fn a_deny_reloaded_while_pending_overrides_the_older_allow_once_decision() {
    let tool = ScriptedTool::succeed("approval_tool", json!({"approved": true}), OrderLog::new());
    let mut registry = ToolRegistry::new();
    registry.register(tool.clone()).expect("register tool");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [tool_step("assistant-tool"), final_step("assistant-final")],
    ));
    let source = Arc::new(super::permission::MutablePermissionStore::default());
    let runtime = super::permission::runtime_with_permission_components(
        source.clone(),
        model,
        registry.snapshot(),
    )
    .await;
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "use the tool".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &session_id).await;
    source.put(
        PermissionFileScope::Session(session_id.clone()),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "rules": [{
                "id": "deny-while-pending",
                "effect": "deny",
                "variants": ["build"],
                "matcher": { "type": "general", "tool_name": "approval_tool" }
            }]
        }))
        .expect("permission JSON"),
    );
    assert!(
        runtime
            .reload_permissions(ReloadPermissionsRequest {
                session_id: session_id.clone(),
            })
            .await
            .expect("reload deny")
            .applied
    );

    runtime
        .decide_approval(DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: ApprovalDecision::AllowOnce,
        })
        .await
        .expect("consume old approval");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert!(tool.executed_inputs().is_empty());
}

#[tokio::test]
async fn runtime_shutdown_cancels_and_removes_pending_approvals() {
    let (runtime, tool) = approval_runtime([tool_step("assistant-tool")]);
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "wait for approval".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    wait_for_pending_approval(&runtime, &session_id).await;

    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown runtime");
    assert_eq!(
        runtime
            .get_run(GetRunRequest {
                session_id: session_id.clone(),
                run_id: run.run_id.clone(),
            })
            .await
            .expect("run query")
            .run
            .status,
        RunStatus::Cancelled
    );
    assert!(tool.executed_inputs().is_empty());
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest { session_id })
            .expect("list pending")
            .approvals
            .is_empty()
    );
}

#[tokio::test]
async fn cancelling_a_run_drops_its_pending_approval_without_executing_the_tool() {
    let (runtime, tool) = approval_runtime([tool_step("assistant-tool")]);
    let mut events = runtime.subscribe_events();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "wait for approval".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            skill_name: None,
            idempotency_key: None,
        })
        .await
        .expect("submit input")
        .run;
    let pending = wait_for_pending_approval(&runtime, &session_id).await;

    runtime
        .cancel_run(CancelRunRequest {
            session_id: session_id.clone(),
            run_id: run.run_id.clone(),
        })
        .await
        .expect("cancel run");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        RunStatus::Cancelled
    );
    assert!(tool.executed_inputs().is_empty());
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session_id.clone(),
            })
            .expect("list pending")
            .approvals
            .is_empty()
    );
    assert!(matches!(
        runtime
            .decide_approval(DecideApprovalRequest {
                session_id,
                approval_id: pending.approval_id,
                decision: ApprovalDecision::AllowOnce,
            })
            .await,
        Err(RuntimeError::ApprovalNotFound { .. })
    ));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, RuntimeEvent::ApprovalCancelled { .. }))
    );
}
