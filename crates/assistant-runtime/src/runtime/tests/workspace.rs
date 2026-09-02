use assistant_protocol::{
    CreateSessionRequest, GetApplicationSnapshotRequest, GetWorkspaceRequest,
    ListWorkspacesRequest, RegisterWorkspaceRequest, RemoveWorkspaceRequest, RuntimeErrorCode,
    SubmitInputRequest, UpdateWorkspaceRequest, WorkspaceLifecycle,
};

use super::*;

#[tokio::test]
async fn workspace_registry_is_idempotent_soft_deleted_and_frozen_into_sessions() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let first_registration = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "project".to_owned(),
            primary_directory: "/workspace/project".to_owned(),
            additional_directories: Vec::new(),
        })
        .await
        .expect("register workspace");
    assert!(!first_registration.restored);
    let first = first_registration.workspace;
    let duplicate_registration = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "project".to_owned(),
            primary_directory: "/workspace/project".to_owned(),
            additional_directories: Vec::new(),
        })
        .await
        .expect("idempotent registration");
    assert!(!duplicate_registration.restored);
    let duplicate = duplicate_registration.workspace;
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
                session_id: bound.session_id.clone(),
            })
            .expect("get bound session")
            .session
            .workspace_id,
        Some(first.workspace_id.clone())
    );
    let application = runtime
        .get_application_snapshot(GetApplicationSnapshotRequest::default())
        .await
        .expect("application snapshot after workspace removal")
        .snapshot
        .value;
    assert!(application.workspaces.is_empty());
    assert!(application.active_sessions.is_empty());

    let error = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(first.workspace_id.clone()),
        })
        .await
        .expect_err("removed workspace must reject new binding");
    assert!(matches!(error, RuntimeError::WorkspaceRemoved { .. }));

    let restored_registration = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "project".to_owned(),
            primary_directory: "/workspace/project".to_owned(),
            additional_directories: Vec::new(),
        })
        .await
        .expect("restore workspace");
    assert!(restored_registration.restored);
    let restored = restored_registration.workspace;
    assert_eq!(restored.workspace_id, first.workspace_id);
    assert_eq!(restored.lifecycle, WorkspaceLifecycle::Active);
    let restored_application = runtime
        .get_application_snapshot(GetApplicationSnapshotRequest::default())
        .await
        .expect("application snapshot after workspace restore")
        .snapshot
        .value;
    assert_eq!(restored_application.workspaces, vec![restored]);
    assert_eq!(restored_application.active_sessions, vec![bound]);
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

struct WorkspacePromptFactory;

#[tokio::test]
async fn workspace_form_rejects_invalid_labels_and_directory_sets_before_storage() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    for label in ["   ".to_owned(), "bad\nlabel".to_owned(), "x".repeat(81)] {
        let error = runtime
            .register_workspace(RegisterWorkspaceRequest {
                label,
                primary_directory: "/workspace/primary".to_owned(),
                additional_directories: Vec::new(),
            })
            .await
            .expect_err("invalid label");
        assert!(matches!(error, RuntimeError::InvalidRequest { .. }));
    }

    let duplicate = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "valid".to_owned(),
            primary_directory: "/workspace/shared".to_owned(),
            additional_directories: vec!["/workspace/shared".to_owned()],
        })
        .await
        .expect_err("duplicate directory");
    assert!(matches!(duplicate, RuntimeError::InvalidRequest { .. }));

    let too_many = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "valid".to_owned(),
            primary_directory: "/workspace/primary".to_owned(),
            additional_directories: (0..16)
                .map(|index| format!("/workspace/additional-{index}"))
                .collect(),
        })
        .await
        .expect_err("more than sixteen total directories");
    assert!(matches!(too_many, RuntimeError::InvalidRequest { .. }));

    assert!(
        runtime
            .list_workspaces(ListWorkspacesRequest::default())
            .expect("list workspaces")
            .workspaces
            .is_empty()
    );
}

impl SessionEnvironmentFactory for WorkspacePromptFactory {
    fn create_environment(
        &self,
        request: SessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        let description = request.workspace.as_ref().map_or_else(
            || "unbound".to_owned(),
            |workspace| {
                format!(
                    "{}|{}|{}",
                    workspace.label,
                    workspace.user_directory,
                    workspace.additional_directories.join("|")
                )
            },
        );
        Ok(test_environment(
            request,
            SystemPromptSnapshot::new(vec![description]),
        ))
    }

    fn create_fork_environment(
        &self,
        request: ForkSessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        Ok(test_fork_environment(request))
    }
}

#[tokio::test]
async fn workspace_update_changes_current_projection_but_not_existing_session_environment() {
    let runtime = runtime_with_factories(
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(WorkspacePromptFactory),
        ToolSetSnapshot::default(),
        32,
    );
    let registered = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "Original".to_owned(),
            primary_directory: "/workspace/primary".to_owned(),
            additional_directories: vec!["/workspace/docs".to_owned()],
        })
        .await
        .expect("register workspace")
        .workspace;
    let existing = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(registered.workspace_id.clone()),
        })
        .await
        .expect("create existing session")
        .session;
    let existing_controller = runtime.session_for_test(&existing.session_id);
    let frozen_environment = existing_controller.environment().clone();
    let frozen_prompt = existing_controller.system_prompt().clone();

    let updated = runtime
        .update_workspace(UpdateWorkspaceRequest {
            workspace_id: registered.workspace_id.clone(),
            label: "Renamed".to_owned(),
            primary_directory: "/workspace/new-primary".to_owned(),
            additional_directories: vec![
                "/workspace/new-docs".to_owned(),
                "/workspace/shared".to_owned(),
            ],
        })
        .await
        .expect("update workspace")
        .workspace;
    assert_eq!(updated.label, "Renamed");
    assert_eq!(existing_controller.environment(), &frozen_environment);
    assert_eq!(existing_controller.system_prompt(), &frozen_prompt);

    let view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: existing.session_id.clone(),
        })
        .await
        .expect("existing session view")
        .snapshot
        .value;
    let context = view.workspace.expect("workspace context");
    assert_eq!(context.label, "Renamed");
    assert_eq!(context.primary_directory, "/workspace/primary");
    assert_eq!(context.additional_directories, vec!["/workspace/docs"]);
    assert!(!context.directories_match_current);

    let controller_session_id = runtime
        .create_session_inner(
            CreateSessionRequest::default(),
            crate::SessionRole::Controller,
            "Controller",
        )
        .await
        .expect("create controller")
        .session
        .session_id;
    let managed = runtime
        .controller_tool_coordinator()
        .list_managed_sessions(&controller_session_id)
        .expect("managed sessions");
    let managed_existing = managed
        .iter()
        .find(|session| session.session_id == existing.session_id.as_str())
        .expect("existing session is managed");
    assert_eq!(managed_existing.workspace_label.as_deref(), Some("Renamed"));
    assert_eq!(
        managed_existing.workspace_primary_directory.as_deref(),
        Some("/workspace/primary")
    );
    assert_eq!(
        managed_existing.workspace_additional_directories,
        vec!["/workspace/docs"]
    );

    let new_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_key: None,
            workspace_id: Some(registered.workspace_id),
        })
        .await
        .expect("create updated session")
        .session;
    let new_controller = runtime.session_for_test(&new_session.session_id);
    assert_eq!(
        new_controller.environment().working_directory,
        "/workspace/new-primary"
    );
    assert_eq!(
        new_controller
            .environment()
            .additional_workspace_directories,
        vec!["/workspace/new-docs", "/workspace/shared"]
    );
    assert!(
        new_controller
            .system_prompt()
            .parts()
            .iter()
            .any(|part| part.contains("Renamed"))
    );
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
            label: "first".to_owned(),
            primary_directory: "/workspace/first".to_owned(),
            additional_directories: Vec::new(),
        })
        .await
        .expect("first workspace")
        .workspace;
    let second_workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "second".to_owned(),
            primary_directory: "/workspace/second".to_owned(),
            additional_directories: Vec::new(),
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
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session_id.clone(),
                message: message.to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
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
            label: "missing".to_owned(),
            primary_directory: "/workspace/missing".to_owned(),
            additional_directories: Vec::new(),
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
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session_id.clone(),
            message: "must fail before start".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
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
        request: crate::RunToolFactoryRequest<'_>,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        let environment = request.environment;
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
