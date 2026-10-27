use assistant_protocol::{
    CreateSessionRequest, GetApplicationSnapshotRequest, GetWorkspaceRequest,
    ListWorkspacesRequest, RegisterWorkspaceRequest, RemoveWorkspaceRequest,
    RestoreWorkspaceRequest, RuntimeErrorCode, SubmitInputRequest, UpdateWorkspaceRequest,
    WorkspaceLifecycle,
};

use crate::{
    SkillPackageSource, SkillScanFuture, SkillScanRequest, SkillScanResult,
    StoredWorkspaceLifecycle, WorkspaceRestoration,
};

use super::*;

#[tokio::test]
async fn workspace_registry_is_idempotent_soft_deleted_and_frozen_into_sessions() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let first_registration = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "project".to_owned(),
            primary_directory: Some("/workspace/project".to_owned()),
            additional_directories: Vec::new(),
        })
        .await
        .expect("register workspace");
    assert!(!first_registration.restored);
    let first = first_registration.workspace;
    let duplicate_registration = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "project".to_owned(),
            primary_directory: Some("/workspace/project".to_owned()),
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
            model_selection: None,
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
            .await
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
            model_selection: None,
            workspace_id: Some(first.workspace_id.clone()),
        })
        .await
        .expect_err("removed workspace must reject new binding");
    assert!(matches!(error, RuntimeError::WorkspaceRemoved { .. }));

    assert_eq!(
        runtime
            .list_workspaces(ListWorkspacesRequest {
                lifecycle: Some(WorkspaceLifecycle::Removed),
            })
            .expect("list removed workspaces")
            .workspaces,
        vec![removed]
    );
    let restored = runtime
        .restore_workspace(RestoreWorkspaceRequest {
            workspace_id: first.workspace_id.clone(),
        })
        .await
        .expect("restore workspace by id")
        .workspace;
    assert_eq!(restored.workspace_id, first.workspace_id);
    assert_eq!(restored.lifecycle, WorkspaceLifecycle::Active);
    assert_eq!(
        runtime
            .restore_workspace(RestoreWorkspaceRequest {
                workspace_id: first.workspace_id.clone(),
            })
            .await
            .expect("repeat restore is idempotent")
            .workspace,
        restored
    );
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
async fn restore_rebuilds_runtime_projection_after_store_committed_first() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "recovery".to_owned(),
            primary_directory: Some("/workspace/recovery".to_owned()),
            additional_directories: Vec::new(),
        })
        .await
        .expect("register workspace")
        .workspace;
    runtime
        .remove_workspace(RemoveWorkspaceRequest {
            workspace_id: workspace.workspace_id.clone(),
        })
        .await
        .expect("remove workspace");

    let committed = runtime
        .store
        .restore_workspace(WorkspaceRestoration {
            workspace_id: workspace.workspace_id.clone(),
            changed_at_ms: 2_000,
        })
        .await
        .expect("commit restore directly in store");
    assert_eq!(committed.lifecycle, StoredWorkspaceLifecycle::Active);
    assert_eq!(committed.updated_at_ms, 2_000);
    assert_eq!(
        runtime
            .list_workspaces(ListWorkspacesRequest {
                lifecycle: Some(WorkspaceLifecycle::Removed),
            })
            .expect("registry remains stale before recovery")
            .workspaces
            .len(),
        1
    );

    let recovered = runtime
        .restore_workspace(RestoreWorkspaceRequest {
            workspace_id: workspace.workspace_id.clone(),
        })
        .await
        .expect("repeat restore rebuilds runtime projection")
        .workspace;
    assert_eq!(recovered.workspace_id, workspace.workspace_id);
    assert_eq!(recovered.lifecycle, WorkspaceLifecycle::Active);
    assert_eq!(recovered.updated_at_ms, 2_000);
    assert_eq!(
        runtime
            .list_workspaces(ListWorkspacesRequest::default())
            .expect("active projection after recovery")
            .workspaces,
        vec![recovered]
    );
}

#[tokio::test]
async fn workspaces_without_user_directories_remain_distinct_and_use_private_working_directories() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let first = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "private".to_owned(),
            primary_directory: None,
            additional_directories: Vec::new(),
        })
        .await
        .expect("register first private workspace")
        .workspace;
    let second = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "private".to_owned(),
            primary_directory: None,
            additional_directories: Vec::new(),
        })
        .await
        .expect("register second private workspace")
        .workspace;
    assert_ne!(first.workspace_id, second.workspace_id);
    assert_eq!(first.user_directory, None);
    assert_eq!(second.user_directory, None);

    let session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
            workspace_id: Some(first.workspace_id.clone()),
        })
        .await
        .expect("create private workspace session")
        .session;
    let environment = runtime
        .session_for_test(&session.session_id)
        .await
        .environment()
        .clone();
    assert_eq!(environment.working_directory, first.agent_directory);
    assert_eq!(
        environment.workspace_private_directory.as_deref(),
        Some(first.agent_directory.as_str())
    );
}

struct FrozenWorkspacePromptFactory;

impl SessionEnvironmentFactory for FrozenWorkspacePromptFactory {
    fn create_environment(
        &self,
        request: SessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        let kind = request.workspace.as_ref().map_or("unbound", |workspace| {
            if workspace.user_directory.is_some() {
                "user"
            } else {
                "private"
            }
        });
        Ok(test_environment(
            request,
            SystemPromptSnapshot::new(vec![kind.to_owned()]),
        ))
    }

    fn create_fork_environment(
        &self,
        request: ForkSessionEnvironmentFactoryRequest<'_>,
    ) -> Result<PreparedSessionEnvironment, SessionEnvironmentFactoryError> {
        Ok(test_fork_environment(request))
    }
}

struct RecordingWorkspaceSkillSource {
    roots: Arc<Mutex<Vec<Vec<String>>>>,
}

impl SkillPackageSource for RecordingWorkspaceSkillSource {
    fn scan(&self, request: SkillScanRequest) -> SkillScanFuture<'_> {
        self.roots
            .lock()
            .expect("skill roots")
            .push(request.workspace_directories);
        Box::pin(async {
            Ok(SkillScanResult {
                complete: true,
                ..SkillScanResult::default()
            })
        })
    }
}

#[tokio::test]
async fn private_workspace_clear_and_skill_refresh_keep_the_sessions_frozen_root() {
    let roots = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = runtime_with_factories(
        Arc::new(StaticModelFactory::new(empty_model())),
        Arc::new(FrozenWorkspacePromptFactory),
        ToolSetSnapshot::default(),
        32,
    );
    runtime.skill_package_source = Arc::new(RecordingWorkspaceSkillSource {
        roots: roots.clone(),
    });
    let workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "private".to_owned(),
            primary_directory: None,
            additional_directories: Vec::new(),
        })
        .await
        .expect("register private workspace")
        .workspace;
    let private_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
            workspace_id: Some(workspace.workspace_id.clone()),
        })
        .await
        .expect("create private session")
        .session;
    let private_controller = runtime.session_for_test(&private_session.session_id).await;
    assert_eq!(
        private_controller
            .current_system_prompt()
            .expect("prompt")
            .parts()
            .last()
            .map(String::as_str),
        Some("private")
    );

    runtime
        .update_workspace(UpdateWorkspaceRequest {
            workspace_id: workspace.workspace_id.clone(),
            label: "external now".to_owned(),
            primary_directory: Some("/workspace/new-external".to_owned()),
            additional_directories: Vec::new(),
        })
        .await
        .expect("edit current workspace");
    runtime
        .current_skill_catalog(&private_controller)
        .await
        .expect("scan old session root");
    assert_eq!(
        roots.lock().expect("skill roots").last(),
        Some(&vec![workspace.agent_directory.clone()])
    );

    runtime
        .clear_session(assistant_protocol::ClearSessionRequest {
            session_id: private_session.session_id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("clear-private-workspace")
                .expect("operation id"),
            expected_generation: 1,
        })
        .await
        .expect("clear frozen private session");
    let cleared = runtime.session_for_test(&private_session.session_id).await;
    assert_eq!(
        cleared.environment().working_directory,
        workspace.agent_directory
    );
    assert_eq!(
        cleared
            .current_system_prompt()
            .expect("prompt")
            .parts()
            .last()
            .map(String::as_str),
        Some("private")
    );

    let current_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
            workspace_id: Some(workspace.workspace_id),
        })
        .await
        .expect("create session after workspace edit")
        .session;
    assert_eq!(
        runtime
            .session_for_test(&current_session.session_id)
            .await
            .environment()
            .working_directory,
        "/workspace/new-external"
    );
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
            model_selection: None,
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
                primary_directory: Some("/workspace/primary".to_owned()),
                additional_directories: Vec::new(),
            })
            .await
            .expect_err("invalid label");
        assert!(matches!(error, RuntimeError::InvalidRequest { .. }));
    }

    let duplicate = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "valid".to_owned(),
            primary_directory: Some("/workspace/shared".to_owned()),
            additional_directories: vec!["/workspace/shared".to_owned()],
        })
        .await
        .expect_err("duplicate directory");
    assert!(matches!(duplicate, RuntimeError::InvalidRequest { .. }));

    // 空字符串与纯空白主目录同样拒绝，不归一化为 None 放行。
    for primary_directory in [String::new(), "   ".to_owned()] {
        let error = runtime
            .register_workspace(RegisterWorkspaceRequest {
                label: "valid".to_owned(),
                primary_directory: Some(primary_directory),
                additional_directories: Vec::new(),
            })
            .await
            .expect_err("blank primary directory");
        assert!(matches!(error, RuntimeError::InvalidRequest { .. }));
    }

    let additional_without_primary = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "valid".to_owned(),
            primary_directory: None,
            additional_directories: vec!["/workspace/additional".to_owned()],
        })
        .await
        .expect_err("additional directories require a primary directory");
    assert!(matches!(
        additional_without_primary,
        RuntimeError::InvalidRequest { .. }
    ));

    let too_many = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "valid".to_owned(),
            primary_directory: Some("/workspace/primary".to_owned()),
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
                    workspace
                        .user_directory
                        .unwrap_or(workspace.agent_directory),
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
            primary_directory: Some("/workspace/primary".to_owned()),
            additional_directories: vec!["/workspace/docs".to_owned()],
        })
        .await
        .expect("register workspace")
        .workspace;
    let existing = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
            workspace_id: Some(registered.workspace_id.clone()),
        })
        .await
        .expect("create existing session")
        .session;
    let existing_controller = runtime.session_for_test(&existing.session_id).await;
    let frozen_environment = existing_controller.environment().clone();
    let frozen_prompt = existing_controller
        .current_system_prompt()
        .expect("system prompt")
        .clone();

    let updated = runtime
        .update_workspace(UpdateWorkspaceRequest {
            workspace_id: registered.workspace_id.clone(),
            label: "Renamed".to_owned(),
            primary_directory: Some("/workspace/new-primary".to_owned()),
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
    assert_eq!(
        existing_controller
            .current_system_prompt()
            .expect("system prompt"),
        frozen_prompt
    );

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
        .await
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
            model_selection: None,
            workspace_id: Some(registered.workspace_id),
        })
        .await
        .expect("create updated session")
        .session;
    let new_controller = runtime.session_for_test(&new_session.session_id).await;
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
            .current_system_prompt()
            .expect("system prompt")
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
            primary_directory: Some(test_absolute_path("/workspace/first")),
            additional_directories: Vec::new(),
        })
        .await
        .expect("first workspace")
        .workspace;
    let second_workspace = runtime
        .register_workspace(RegisterWorkspaceRequest {
            label: "second".to_owned(),
            primary_directory: Some(test_absolute_path("/workspace/second")),
            additional_directories: Vec::new(),
        })
        .await
        .expect("second workspace")
        .workspace;
    let first_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
            workspace_id: Some(first_workspace.workspace_id),
        })
        .await
        .expect("first session")
        .session;
    let second_session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
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
            .submit_started_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session_id.clone(),
                message: message.to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
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
            test_absolute_path("/workspace/first"),
            test_absolute_path("/workspace/second")
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
            primary_directory: Some("/workspace/missing".to_owned()),
            additional_directories: Vec::new(),
        })
        .await
        .expect("workspace")
        .workspace;
    let session = runtime
        .create_session(CreateSessionRequest {
            title: None,
            model_selection: None,
            workspace_id: Some(workspace.workspace_id),
        })
        .await
        .expect("session")
        .session;
    let accepted = runtime
        .submit_started_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session.session_id.clone(),
            message: "must fail before start".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
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
    let conversation = runtime
        .conversation_snapshot(&session.session_id)
        .await
        .expect("failed input conversation");
    assert_eq!(conversation.messages.len(), 1);
    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session_id,
            })
            .await
            .expect("session")
            .session
            .queued_input_count,
        0
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
