use super::*;
use assistant_protocol::ShellKind;

async fn conversation_items(
    runtime: &AssistantRuntime,
    session_id: &SessionId,
) -> Vec<assistant_protocol::ConversationItem> {
    runtime
        .list_conversation_page(assistant_protocol::ListConversationPageRequest {
            owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: session_id.clone(),
            },
            cursor: None,
            limit: 30,
        })
        .await
        .unwrap()
        .snapshot
        .value
        .items
}

struct FirstCallGate {
    inner: ScriptedModelService,
    first: AtomicBool,
    entered: Notify,
    release: Notify,
}

impl ModelService for FirstCallGate {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }
    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }
    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                tokio::select! {
                    () = self.release.notified() => {},
                    () = context.cancellation.cancelled() => return Err(ModelError::Cancelled),
                }
            }
            self.inner.stream(request, context).await
        })
    }
}

#[tokio::test]
async fn shell_switch_waits_for_active_run_and_precedes_later_input() {
    verify_queued_shell_change(ShellKind::Powershell7).await;
}

#[tokio::test]
async fn explicit_shell_refresh_waits_for_the_active_run() {
    verify_queued_shell_change(ShellKind::WindowsPowershell51).await;
}

async fn verify_queued_shell_change(target: ShellKind) {
    let model = Arc::new(FirstCallGate {
        inner: ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            ["before", "after"]
                .map(|id| ModelScript::Events(message_events(&assistant_text(id, "done")))),
        ),
        first: AtomicBool::new(true),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let factory = Arc::new(ShellFactory::default());
    let runtime = runtime_with_run_tool_factory(model.clone(), factory.clone());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let request = SubmitInputRequest {
        session_id: session_id.clone(),
        message: "normal input".to_owned(),
        variant: assistant_protocol::AgentVariant::Build,
        mode: assistant_protocol::SubmitInputMode::Normal,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    };
    let before = runtime.submit_input(request.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), model.entered.notified())
        .await
        .unwrap();
    factory.environment_changed.store(true, Ordering::SeqCst);
    let switch = runtime
        .submit_session_command(assistant_protocol::SubmitSessionCommandRequest {
            session_id: session_id.clone(),
            command: assistant_protocol::SessionCommand::AgentShellSwitch { shell: target },
            idempotency_key: None,
        })
        .await
        .unwrap();
    let after = runtime.submit_input(request).await.unwrap();
    let view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(view.agent_shell_kind, Some(ShellKind::WindowsPowershell51));
    let targets = view
        .queue
        .items
        .iter()
        .filter_map(|item| match item {
            assistant_protocol::QueuedSessionItemSnapshot::Command(input) => match input.command {
                assistant_protocol::SessionCommand::AgentShellSwitch { shell } => Some(shell),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(targets, [target]);
    let queued = runtime.store.load_session_state(&session_id).await.unwrap();
    assert_eq!(
        queued.state.sessions[0].agent_shell_kind,
        Some(ShellKind::WindowsPowershell51)
    );
    assert_eq!(factory.compiled.lock().unwrap().len(), 1);
    assert!(
        queued
            .state
            .runs
            .iter()
            .filter(|run| run.run_id != before.run.run_id)
            .all(|run| run.shell.is_none())
    );
    assert_eq!(
        factory.probes.load(Ordering::SeqCst),
        1,
        "queued changes must not probe during the active Run"
    );
    model.release.notify_one();
    assert!(switch.accepted.run_id.is_none());
    for run_id in [&before.run.run_id, &after.run.run_id] {
        assert_eq!(
            wait_for_terminal(&runtime, &session_id, run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
    }
    let compiled = factory
        .compiled
        .lock()
        .unwrap()
        .iter()
        .map(|shell| shell.kind)
        .collect::<Vec<_>>();
    let view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(view.agent_shell_kind, Some(target));
    assert!(view.queue.items.is_empty());
    assert_eq!(compiled, [ShellKind::WindowsPowershell51, target]);
    let stored = runtime.store.load_session_state(&session_id).await.unwrap();
    let snapshots = factory.compiled.lock().unwrap().clone();
    assert_eq!(snapshots[0].program, "fixture-shell.exe");
    assert_eq!(snapshots[1].program, "refreshed-shell.exe");
    assert_eq!(model.inner.take_requests().len(), 2);
    assert_eq!(stored.state.runs.len(), 2);
    for (id, expected) in [
        (&before.run.run_id, ShellKind::WindowsPowershell51),
        (&after.run.run_id, target),
    ] {
        assert_eq!(
            stored
                .state
                .runs
                .iter()
                .find(|run| &run.run_id == id)
                .unwrap()
                .shell
                .as_ref()
                .unwrap()
                .kind,
            expected
        );
    }
    assert_eq!(
        factory.probes.load(Ordering::SeqCst),
        2,
        "only initialization and the explicit change probe"
    );
    runtime.shutdown(Default::default()).await.unwrap();
}

#[derive(Default)]
struct ShellFactory {
    environment_changed: AtomicBool,
    probes: std::sync::atomic::AtomicUsize,
    compiled: Mutex<Vec<crate::FrozenShellEnvironment>>,
    git_bash_available: AtomicBool,
}

async fn wait_for_shell_command(
    runtime: &AssistantRuntime,
    session_id: &SessionId,
    input_id: &assistant_protocol::InputId,
) -> crate::StoredSessionCommand {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let session = runtime.session(session_id).await.unwrap();
            let command = session
                .lock_state()
                .unwrap()
                .commands
                .get(input_id)
                .cloned()
                .unwrap();
            if command.state == crate::StoredSessionCommandState::Committed {
                return command;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn shell_control_failure_retry_and_forks_never_call_the_model() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "after-controls",
            "done",
        )))],
    ));
    let factory = Arc::new(ShellFactory::default());
    let runtime = runtime_with_run_tool_factory(model.clone(), factory.clone());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let request = assistant_protocol::SubmitSessionCommandRequest {
        session_id: session_id.clone(),
        command: assistant_protocol::SessionCommand::AgentShellSwitch {
            shell: ShellKind::GitBash,
        },
        idempotency_key: Some(assistant_protocol::IdempotencyKey::new("shell-control").unwrap()),
    };
    let first = runtime
        .submit_session_command(request.clone())
        .await
        .unwrap();
    assert!(first.accepted.run_id.is_none());
    let failed = wait_for_shell_command(&runtime, &session_id, &first.accepted.input_id).await;
    assert!(matches!(
        failed.result,
        Some(crate::StoredSessionCommandResult::ShellSwitch {
            environment: None,
            error: Some(_),
            ..
        })
    ));
    assert_eq!(
        runtime
            .session(&session_id)
            .await
            .unwrap()
            .lock_state()
            .unwrap()
            .agent_shell_kind,
        Some(ShellKind::WindowsPowershell51)
    );
    let duplicate = runtime
        .submit_session_command(request.clone())
        .await
        .unwrap();
    assert!(duplicate.accepted.is_duplicate);
    assert_eq!(duplicate.accepted.input_id, first.accepted.input_id);
    let mut conflicting = request.clone();
    conflicting.command = assistant_protocol::SessionCommand::AgentShellSwitch {
        shell: ShellKind::Cmd,
    };
    assert!(runtime.submit_session_command(conflicting).await.is_err());
    factory.git_bash_available.store(true, Ordering::SeqCst);
    let retried = runtime
        .submit_session_command(assistant_protocol::SubmitSessionCommandRequest {
            idempotency_key: None,
            ..request
        })
        .await
        .unwrap();
    let success = wait_for_shell_command(&runtime, &session_id, &retried.accepted.input_id).await;
    assert!(matches!(
        success.result,
        Some(crate::StoredSessionCommandResult::ShellSwitch {
            environment: Some(_),
            error: None,
            ..
        })
    ));
    assert!(model.take_requests().is_empty());
    let ordinary = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "after controls".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &ordinary.run.run_id).await;
    assert_eq!(model.take_requests().len(), 1);
    let mut source = session_id;
    for _ in 0..3 {
        let items = conversation_items(&runtime, &source).await;
        let results = items
            .iter()
            .filter_map(|item| match item {
                assistant_protocol::ConversationItem::ShellSwitchResult {
                    run_id,
                    success,
                    previous_shell,
                    ..
                } => {
                    assert!(run_id.is_none());
                    assert_eq!(*previous_shell, Some(ShellKind::WindowsPowershell51));
                    Some(*success)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(results, [false, true]);
        let stored = runtime.store.load_session_state(&source).await.unwrap();
        assert!(stored.state.runs.len() <= 1);
        assert_eq!(
            stored.state.sessions[0].agent_shell_kind,
            Some(ShellKind::GitBash)
        );
        let generation = runtime
            .session(&source)
            .await
            .unwrap()
            .lock_state()
            .unwrap()
            .body_generation;
        source = runtime
            .fork_session(assistant_protocol::ForkSessionRequest {
                session_id: source,
                fork_point: assistant_protocol::MessageId::new("after-controls").unwrap(),
                expected_generation: generation,
            })
            .await
            .unwrap()
            .session
            .session_id;
    }
    assert!(model.take_requests().is_empty());
    runtime.shutdown(Default::default()).await.unwrap();
}

impl RunToolFactory for ShellFactory {
    fn shell_catalog(&self) -> Vec<assistant_protocol::ShellCatalogEntry> {
        [
            ShellKind::WindowsPowershell51,
            ShellKind::Cmd,
            ShellKind::Powershell7,
            ShellKind::GitBash,
        ]
        .into_iter()
        .map(|kind| {
            let available = self.freeze_shell(Some(kind)).is_ok();
            assistant_protocol::ShellCatalogEntry {
                kind,
                available,
                reason: (!available).then(|| "fixture unavailable".to_owned()),
            }
        })
        .collect()
    }
    fn freeze_shell(
        &self,
        kind: Option<ShellKind>,
    ) -> Result<Option<crate::FrozenShellEnvironment>, RunToolFactoryError> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        let kind = kind.unwrap_or(ShellKind::WindowsPowershell51);
        if kind == ShellKind::GitBash && !self.git_bash_available.load(Ordering::SeqCst) {
            return Err(RunToolFactoryError::new(
                crate::RunToolFactoryErrorKind::InvalidConfiguration,
            ));
        }
        Ok(Some(crate::FrozenShellEnvironment {
            kind,
            operating_system: "windows".to_owned(),
            program: if self.environment_changed.load(Ordering::SeqCst) {
                "refreshed-shell.exe"
            } else {
                "fixture-shell.exe"
            }
            .to_owned(),
            fixed_args: vec!["-Command".to_owned()],
            command_prefix: String::new(),
            dialect: "fixture".to_owned(),
        }))
    }

    fn compile(
        &self,
        request: crate::RunToolFactoryRequest<'_>,
    ) -> Result<RunToolBundle, RunToolFactoryError> {
        if let Some(shell) = request.shell {
            self.compiled.lock().unwrap().push(shell.clone());
        }
        Ok(RunToolBundle::new(ToolSetSnapshot::default(), Vec::new()))
    }
}

#[tokio::test]
async fn shell_preparation_failure_atomically_commits_the_failed_input() {
    let runtime = runtime_with_run_tool_factory(empty_model(), Arc::new(ShellFactory::default()));
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    {
        let controller = runtime
            .session(&session.session_id)
            .await
            .expect("controller");
        let mut state = controller.lock_state().expect("state");
        state.agent_shell_kind = Some(ShellKind::GitBash);
        state.agent_shell_environment = None;
    }
    let accepted = runtime
        .submit_input(SubmitInputRequest {
            session_id: session.session_id.clone(),
            message: "shell failure must preserve this input".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("accepted input");
    assert_eq!(
        wait_for_terminal(&runtime, &session.session_id, &accepted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Failed
    );
    assert_eq!(
        runtime
            .conversation_snapshot(&session.session_id)
            .await
            .expect("failed input conversation")
            .messages
            .len(),
        1
    );
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

#[tokio::test]
async fn new_sessions_freeze_current_default_and_missing_target_does_not_fall_back() {
    let runtime = runtime_with_run_tool_factory(empty_model(), Arc::new(ShellFactory::default()));
    let settings = runtime.get_agent_shell_settings().await.unwrap();
    assert_eq!(settings.default_agent_shell, None);
    assert_eq!(settings.catalog.len(), 4);
    assert!(
        settings
            .catalog
            .iter()
            .any(|entry| entry.kind == ShellKind::GitBash
                && !entry.available
                && entry.reason.is_some())
    );
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session;
    assert_eq!(
        runtime
            .session(&first.session_id)
            .await
            .unwrap()
            .lock_state()
            .unwrap()
            .agent_shell_kind,
        Some(ShellKind::WindowsPowershell51)
    );
    let settings = runtime
        .set_default_agent_shell(ShellKind::Powershell7)
        .await
        .unwrap();
    assert_eq!(settings.default_agent_shell, Some(ShellKind::Powershell7));
    assert!(
        runtime
            .set_default_agent_shell(ShellKind::GitBash)
            .await
            .is_err()
    );
    assert_eq!(
        runtime
            .get_agent_shell_settings()
            .await
            .unwrap()
            .default_agent_shell,
        Some(ShellKind::Powershell7)
    );
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session;
    assert_eq!(
        runtime
            .session(&second.session_id)
            .await
            .unwrap()
            .lock_state()
            .unwrap()
            .agent_shell_kind,
        Some(ShellKind::Powershell7)
    );
    assert_eq!(
        runtime
            .session(&first.session_id)
            .await
            .unwrap()
            .lock_state()
            .unwrap()
            .agent_shell_kind,
        Some(ShellKind::WindowsPowershell51)
    );
    runtime
        .store
        .save_default_agent_shell(ShellKind::GitBash)
        .await
        .unwrap();
    assert!(matches!(
        runtime
            .create_session(CreateSessionRequest::default())
            .await,
        Err(RuntimeError::RunToolsBuildFailed { .. })
    ));
    assert_eq!(
        runtime.store.load_default_agent_shell().await.unwrap(),
        Some(ShellKind::GitBash)
    );
    runtime.shutdown(Default::default()).await.unwrap();
}

#[tokio::test]
async fn shell_binding_survives_archive_restore_and_clear_after_default_changes() {
    let factory = Arc::new(ShellFactory::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        ["before-archive", "after-restore", "after-clear"]
            .map(|id| ModelScript::Events(message_events(&assistant_text(id, "done")))),
    ));
    let runtime = runtime_with_run_tool_factory(model.clone(), factory.clone());
    runtime
        .set_default_agent_shell(ShellKind::Powershell7)
        .await
        .unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    for stage in 0..3 {
        if stage == 1 {
            runtime
                .archive_session(assistant_protocol::ArchiveSessionRequest {
                    session_id: session_id.clone(),
                })
                .await
                .unwrap();
            runtime
                .set_default_agent_shell(ShellKind::Cmd)
                .await
                .unwrap();
            assert_eq!(
                runtime
                    .store
                    .load_session_state(&session_id)
                    .await
                    .unwrap()
                    .state
                    .sessions[0]
                    .agent_shell_kind,
                Some(ShellKind::Powershell7)
            );
            runtime
                .restore_session(assistant_protocol::RestoreSessionRequest {
                    session_id: session_id.clone(),
                })
                .await
                .unwrap();
        } else if stage == 2 {
            runtime
                .clear_session(assistant_protocol::ClearSessionRequest {
                    session_id: session_id.clone(),
                    operation_id: assistant_protocol::IdempotencyKey::new("clear-shell-binding")
                        .unwrap(),
                    expected_generation: 1,
                })
                .await
                .unwrap();
            assert!(
                runtime
                    .conversation_snapshot(&session_id)
                    .await
                    .unwrap()
                    .messages
                    .is_empty()
            );
        }
        let accepted = runtime
            .submit_input(SubmitInputRequest {
                session_id: session_id.clone(),
                message: format!("shell lifecycle stage {stage}"),
                variant: assistant_protocol::AgentVariant::Build,
                mode: assistant_protocol::SubmitInputMode::Normal,
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
        let stored = runtime.store.load_session_state(&session_id).await.unwrap();
        assert_eq!(
            stored.state.sessions[0].agent_shell_kind,
            Some(ShellKind::Powershell7)
        );
        let run = stored
            .state
            .runs
            .iter()
            .find(|run| run.run_id == accepted.run.run_id)
            .unwrap();
        assert_eq!(run.shell.as_ref().unwrap().kind, ShellKind::Powershell7);
    }
    let compiled = factory.compiled.lock().unwrap().clone();
    assert_eq!(compiled.len(), 3);
    assert!(compiled.iter().all(|shell| shell == &compiled[0]));
    let expected = compiled[0]
        .context_message(
            &runtime
                .session(&session_id)
                .await
                .unwrap()
                .environment()
                .working_directory,
        )
        .unwrap();
    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert!(crate::shell::context_is_current(
            &request.conversation,
            &expected
        ));
    }
    assert!(!requests[2].conversation.messages.iter().any(|message| {
        matches!(message, agent_types::ConversationMessage::Assistant(message)
            if message.id.as_str() == "before-archive" || message.id.as_str() == "after-restore")
    }));
    runtime.shutdown(Default::default()).await.unwrap();
}

#[tokio::test]
async fn run_persists_the_exact_shell_used_to_compile_tools() {
    let factory = Arc::new(ShellFactory::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        ["shell-answer", "shell-second", "shell-switched"]
            .map(|id| ModelScript::Events(message_events(&assistant_text(id, "done")))),
    ));
    let runtime = runtime_with_run_tool_factory(model.clone(), factory.clone());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    runtime
        .store
        .save_default_agent_shell(ShellKind::Cmd)
        .await
        .unwrap();
    let request = SubmitInputRequest {
        session_id: session_id.clone(),
        message: "use the bound shell".to_owned(),
        variant: assistant_protocol::AgentVariant::Build,
        mode: assistant_protocol::SubmitInputMode::Normal,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    };
    let probes_before_input = factory.probes.load(Ordering::SeqCst);
    let accepted = runtime.submit_input(request.clone()).await.unwrap();
    let terminal = wait_for_terminal(&runtime, &session_id, &accepted.run.run_id).await;
    assert_eq!(terminal.status, assistant_protocol::RunStatus::Completed);
    let restored = runtime.store.load_session_state(&session_id).await.unwrap();
    let run = restored
        .state
        .runs
        .iter()
        .find(|run| run.run_id == accepted.run.run_id)
        .unwrap();
    let compiled = factory.compiled.lock().unwrap().clone();
    assert_eq!(compiled.len(), 1);
    assert_eq!(compiled[0].kind, ShellKind::WindowsPowershell51);
    assert_eq!(run.shell.as_ref(), Some(&compiled[0]));
    let conversation = runtime.store.load_conversation(&session_id).await.unwrap();
    let expected = compiled[0]
        .context_message(
            &runtime
                .session(&session_id)
                .await
                .unwrap()
                .environment()
                .working_directory,
        )
        .unwrap();
    assert!(crate::shell::context_is_current(&conversation, &expected));
    factory.environment_changed.store(true, Ordering::SeqCst);
    let second = runtime.submit_input(request.clone()).await.unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &second.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(
        factory.probes.load(Ordering::SeqCst),
        probes_before_input,
        "ordinary inputs must reuse the confirmed environment"
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(crate::shell::context_is_current(
            &request.conversation,
            &expected
        ));
        let count = request.conversation.messages.iter().filter_map(|message| {
            match message {
                ConversationMessage::User(message) => Some(&message.parts),
                _ => None,
            }
        }).flatten().filter(|part| matches!(part, UserPart::InternalContext(part) if part.kind == "shell_environment")).count();
        assert_eq!(count, 1, "unchanged environment must not be injected again");
    }
    runtime.shutdown(Default::default()).await.unwrap();
}

#[tokio::test]
async fn shell_context_returns_after_real_compaction_without_changing_the_binding() {
    let factory = Arc::new(ShellFactory::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            "before-one",
            "before-two",
            "compact-summary",
            "after-compaction",
        ]
        .map(|id| ModelScript::Events(message_events(&assistant_text(id, "completed")))),
    ));
    let runtime = runtime_with_run_tool_factory(model.clone(), factory.clone());
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let request = SubmitInputRequest {
        session_id: session_id.clone(),
        message: "execute in this Shell".into(),
        variant: assistant_protocol::AgentVariant::Build,
        mode: assistant_protocol::SubmitInputMode::Normal,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    };
    for _ in 0..2 {
        let accepted = runtime.submit_input(request.clone()).await.unwrap();
        assert_eq!(
            wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
    }
    let frozen = factory.compiled.lock().unwrap()[0].clone();
    let expected = frozen
        .context_message(
            &runtime
                .session(&session_id)
                .await
                .unwrap()
                .environment()
                .working_directory,
        )
        .unwrap();
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    let compacted = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("shell-context-compaction")
                .unwrap(),
            expected_generation: view.conversation_generation,
        })
        .await
        .unwrap();
    assert!(matches!(
        compacted.outcome,
        assistant_protocol::CompactSessionOutcome::Compacted { .. }
    ));
    let conversation = runtime.conversation_snapshot(&session_id).await.unwrap();
    assert!(
        !crate::shell::context_is_current(&conversation, &expected),
        "the older environment must actually leave the effective context"
    );
    let accepted = runtime.submit_input(request).await.unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 4);
    assert!(crate::shell::context_is_current(
        &requests[3].conversation,
        &expected
    ));
    let stored = runtime.store.load_session_state(&session_id).await.unwrap();
    assert_eq!(stored.state.sessions[0].agent_shell_kind, Some(frozen.kind));
    assert!(
        factory
            .compiled
            .lock()
            .unwrap()
            .iter()
            .all(|actual| actual == &frozen)
    );
    runtime.shutdown(Default::default()).await.unwrap();
}

#[tokio::test]
async fn shell_context_survives_automatic_threshold_compaction() {
    verify_shell_after_automatic_compaction(false).await;
}

#[tokio::test]
async fn shell_context_survives_provider_overflow_compaction() {
    verify_shell_after_automatic_compaction(true).await;
}

async fn verify_shell_after_automatic_compaction(provider_overflow: bool) {
    let factory = Arc::new(ShellFactory::default());
    let mut first = assistant_text("shell-before-automatic", "prior answer");
    if !provider_overflow {
        first.usage = Some(agent_types::TokenUsage {
            input_tokens: 6_500,
            output_tokens: 500,
            total_tokens: 7_000,
            cached_input_tokens: Some(6_000),
            reasoning_tokens: None,
        });
    }
    let mut scripts = vec![ModelScript::Events(message_events(&first))];
    if provider_overflow {
        scripts.push(ModelScript::FailEstablishment(
            ModelError::ContextOverflow {
                message: "shell fixture overflow".into(),
            },
        ));
    }
    scripts.extend(
        ["shell-automatic-summary", "shell-after-automatic"]
            .map(|id| ModelScript::Events(message_events(&assistant_text(id, "completed")))),
    );
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        scripts,
    ));
    let runtime = runtime_with_run_tool_factory(model.clone(), factory.clone());
    runtime
        .set_default_agent_shell(ShellKind::Powershell7)
        .await
        .unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let mut events = runtime.subscribe_events();
    for message in ["first turn", "continue after automatic compaction"] {
        let accepted = runtime
            .submit_input(SubmitInputRequest {
                session_id: session_id.clone(),
                message: message.into(),
                variant: assistant_protocol::AgentVariant::Build,
                mode: assistant_protocol::SubmitInputMode::Normal,
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
    }
    let frozen = factory.compiled.lock().unwrap()[0].clone();
    let expected = frozen
        .context_message(
            &runtime
                .session(&session_id)
                .await
                .unwrap()
                .environment()
                .working_directory,
        )
        .unwrap();
    let requests = model.take_requests();
    assert_eq!(requests.len(), if provider_overflow { 4 } else { 3 });
    let resumed = &requests.last().unwrap().conversation;
    // 必须实际走过 generation 替换；仅模型调用成功无法证明压缩后环境被补回。
    assert!(matches!(
        resumed.messages.first(),
        Some(ConversationMessage::ContextSummary(_))
    ));
    assert!(crate::shell::context_is_current(resumed, &expected));
    let mut compacted = false;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event,
            RuntimeEvent::SessionCompactionFinished {
                outcome: assistant_protocol::SessionCompactionFinishedOutcome::Compacted { .. },
                ..
            }
        ) {
            compacted = true;
        }
    }
    assert!(compacted);
    let stored = runtime.store.load_session_state(&session_id).await.unwrap();
    assert_eq!(
        stored.state.sessions[0].agent_shell_kind,
        Some(ShellKind::Powershell7)
    );
    assert_eq!(
        stored.state.runs.len(),
        2,
        "compaction resumes the existing Run"
    );
    assert!(
        stored
            .state
            .runs
            .iter()
            .all(|run| run.shell.as_ref() == Some(&frozen))
    );
    assert!(
        factory
            .compiled
            .lock()
            .unwrap()
            .iter()
            .all(|shell| shell == &frozen)
    );
    runtime.shutdown(Default::default()).await.unwrap();
}

#[tokio::test]
async fn delegated_model_receives_the_parent_run_shell_and_execution_directory() {
    let factory = Arc::new(ShellFactory::default());
    let mut delegate = assistant_text("delegate-shell", "");
    delegate.parts = vec![AssistantPart::ToolCall(ToolCall {
        id: ToolCallId::new("delegate-shell-call").unwrap(),
        name: ToolName::new("delegate_task").unwrap(),
        arguments: json!({ "title": "Check Shell", "task": "Report the execution environment." }),
    })];
    delegate.finish_reason = FinishReason::ToolCalls;
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&delegate)),
            ModelScript::Events(message_events(&assistant_text(
                "child-shell-answer",
                "child done",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "parent-shell-answer",
                "parent done",
            ))),
        ],
    ));
    let runtime = runtime_with_run_tool_factory(model.clone(), factory.clone());
    runtime
        .set_default_agent_shell(ShellKind::Powershell7)
        .await
        .unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &session_id).await;
    let accepted = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "delegate environment inspection".into(),
            variant: assistant_protocol::AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]
            .tools
            .iter()
            .all(|tool| tool.name.as_str() != "delegate_task")
    );
    let frozen = factory.compiled.lock().unwrap()[0].clone();
    assert_eq!(frozen.kind, ShellKind::Powershell7);
    let session = runtime.session(&session_id).await.unwrap();
    let cwd = &session.environment().working_directory;
    let expected = frozen.context_message(cwd).unwrap();
    for request in &requests {
        assert!(crate::shell::context_is_current(
            &request.conversation,
            &expected
        ));
    }
    assert!(requests[1].conversation.messages.iter().any(|message| match message {
        ConversationMessage::User(message) => message.parts.iter().any(|part| matches!(part,
            UserPart::InternalContext(part) if part.kind == "delegation_directories" && part.text.contains(cwd))),
        _ => false,
    }));
    runtime.shutdown(Default::default()).await.unwrap();
}
