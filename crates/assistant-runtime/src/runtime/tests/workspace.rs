use assistant_protocol::{
    CreateSessionRequest, GetWorkspaceRequest, ListWorkspacesRequest, RegisterWorkspaceRequest,
    RemoveWorkspaceRequest, RuntimeErrorCode, SetEmptySessionWorkspaceRequest, SubmitInputRequest,
    WorkspaceLifecycle,
};

use super::*;

#[tokio::test]
async fn workspace_registry_is_idempotent_soft_deleted_and_frozen_into_sessions() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let first = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/project".to_owned(),
        })
        .await
        .expect("register workspace")
        .workspace;
    let duplicate = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/project".to_owned(),
        })
        .await
        .expect("idempotent registration")
        .workspace;
    assert_eq!(duplicate.workspace_id, first.workspace_id);

    let bound = runtime
        .create_session(CreateSessionRequest {
            title: Some("Bound".to_owned()),
            model_key: None,
            workspace_id: Some(first.workspace_id.clone()),
        })
        .await
        .expect("create bound session")
        .session;
    assert_eq!(bound.workspace_id.as_ref(), Some(&first.workspace_id));

    let removed = runtime
        .remove_workspace(RemoveWorkspaceRequest {
            workspace_id: first.workspace_id.clone(),
        })
        .await
        .expect("remove workspace")
        .workspace;
    assert_eq!(removed.lifecycle, WorkspaceLifecycle::Removed);
    assert!(
        runtime
            .list_workspaces(ListWorkspacesRequest::default())
            .expect("list workspaces")
            .workspaces
            .is_empty()
    );
    assert_eq!(
        runtime
            .get_workspace(GetWorkspaceRequest {
                workspace_id: first.workspace_id.clone(),
            })
            .expect("get removed workspace")
            .workspace
            .lifecycle,
        WorkspaceLifecycle::Removed
    );
    assert_eq!(
        runtime
            .get_session(assistant_protocol::GetSessionRequest {
                session_id: bound.session_id,
            })
            .expect("get bound session")
            .session
            .workspace_id,
        Some(first.workspace_id.clone())
    );

    let error = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(first.workspace_id.clone()),
        })
        .await
        .expect_err("removed workspace must reject new binding");
    assert!(matches!(error, RuntimeError::WorkspaceRemoved { .. }));

    let restored = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/project".to_owned(),
        })
        .await
        .expect("restore workspace")
        .workspace;
    assert_eq!(restored.workspace_id, first.workspace_id);
    assert_eq!(restored.lifecycle, WorkspaceLifecycle::Active);
}

#[tokio::test]
async fn unbound_session_remains_supported_and_unknown_workspace_is_structured() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let unbound = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("create unbound session")
        .session;
    assert_eq!(unbound.workspace_id, None);

    let error = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(
                assistant_protocol::WorkspaceId::new("w-missing").expect("workspace id"),
            ),
        })
        .await
        .expect_err("unknown workspace");
    assert!(matches!(error, RuntimeError::WorkspaceNotFound { .. }));
}

#[tokio::test]
async fn only_a_completely_empty_session_can_rebind_its_workspace() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let first = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/first".to_owned(),
        })
        .await
        .expect("first workspace")
        .workspace;
    let second = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/second".to_owned(),
        })
        .await
        .expect("second workspace")
        .workspace;
    let session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(first.workspace_id),
        })
        .await
        .expect("session")
        .session;
    let rebound = runtime
        .set_empty_session_workspace(SetEmptySessionWorkspaceRequest {
            session_id: session.session_id.clone(),
            workspace_id: Some(second.workspace_id.clone()),
        })
        .await
        .expect("rebind empty session")
        .session;
    assert_eq!(rebound.workspace_id, Some(second.workspace_id));

    runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session_id.clone(),
            message: "now nonempty".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("input");
    assert!(matches!(
        runtime
            .set_empty_session_workspace(SetEmptySessionWorkspaceRequest {
                session_id: session.session_id,
                workspace_id: None,
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
}

#[tokio::test]
async fn every_run_compiles_tools_from_its_sessions_frozen_workspace() {
    let model: Arc<dyn ModelService> = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("answer-one", "one"))),
            ModelScript::Events(message_events(&assistant_text("answer-two", "two"))),
        ],
    ));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let runtime = runtime_with_run_tool_factory(
        model,
        Arc::new(ObservingRunToolFactory {
            observed: observed.clone(),
            fail_workdir: false,
        }),
    );
    let first_workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/first".to_owned(),
        })
        .await
        .expect("first workspace")
        .workspace;
    let second_workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/second".to_owned(),
        })
        .await
        .expect("second workspace")
        .workspace;
    let first_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(first_workspace.workspace_id),
        })
        .await
        .expect("first session")
        .session;
    let second_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(second_workspace.workspace_id),
        })
        .await
        .expect("second session")
        .session;
    for (session_id, message) in [
        (first_session.session_id, "first"),
        (second_session.session_id, "second"),
    ] {
        let accepted = runtime
            .submit_input(SubmitInputRequest {
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session_id.clone(),
                message: message.to_owned(),
                attachment_ids: Vec::new(),
                idempotency_key: None,
            })
            .await
            .expect("submit input");
        assert_eq!(
            wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
    }
    assert_eq!(
        *observed.lock().expect("observed environments"),
        vec![
            "/workspace/first".to_owned(),
            "/workspace/second".to_owned()
        ]
    );
}

#[tokio::test]
async fn missing_bound_workdir_is_reported_as_workspace_unavailable_before_start() {
    let runtime = runtime_with_run_tool_factory(
        empty_model(),
        Arc::new(ObservingRunToolFactory {
            observed: Arc::new(Mutex::new(Vec::new())),
            fail_workdir: true,
        }),
    );
    let workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            path: "/workspace/missing".to_owned(),
        })
        .await
        .expect("workspace")
        .workspace;
    let session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(workspace.workspace_id),
        })
        .await
        .expect("session")
        .session;
    let accepted = runtime
        .submit_input(SubmitInputRequest {
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session_id.clone(),
            message: "must fail before start".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
        })
        .await
        .expect("input accepted");
    let failed = wait_for_terminal(&runtime, &session.session_id, &accepted.run.run_id).await;
    assert_eq!(failed.status, assistant_protocol::RunStatus::Failed);
    assert_eq!(
        failed.error.expect("structured failure").code,
        RuntimeErrorCode::WorkspaceUnavailable
    );
}

struct ObservingRunToolFactory {
    observed: Arc<Mutex<Vec<String>>>,
    fail_workdir: bool,
}

impl RunToolFactory for ObservingRunToolFactory {
    fn compile(
        &self,
        environment: &SessionExecutionEnvironment,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        self.observed
            .lock()
            .expect("observed environments")
            .push(environment.working_directory.clone());
        if self.fail_workdir {
            return Err(RunToolFactoryError::new(
                RunToolFactoryErrorKind::WorkingDirectoryUnavailable,
            ));
        }
        Ok(RunToolBundle::new(ToolSetSnapshot::default(), Vec::new()))
    }
}
