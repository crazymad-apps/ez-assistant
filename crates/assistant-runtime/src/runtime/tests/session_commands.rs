//! M5 控制指令只使用易失 Store 与进程内 MCP fixture，不接触真实配置或服务。

use super::product::{
    StaticMcpTestSource, WorkingMcpTestConnection, WorkingMcpTestFactory, mcp_tool_step,
};
use super::*;
use assistant_protocol::{
    ConversationItem, IdempotencyKey, InputId, McpRefreshOutcome, McpServerKey,
    QueuedSessionItemSnapshot, ResumeQueuedInputRequest, SessionCommand, SessionCommandQueueState,
    SubmitSessionCommandRequest,
};

fn with_mcp(runtime: AssistantRuntime) -> AssistantRuntime {
    runtime.with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory {
            connection: Arc::new(WorkingMcpTestConnection {
                calls: AtomicUsize::new(0),
            }),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    )
}

fn refresh_request(session_id: &SessionId, key: &str) -> SubmitSessionCommandRequest {
    SubmitSessionCommandRequest {
        session_id: session_id.clone(),
        command: SessionCommand::McpRefresh { server: None },
        idempotency_key: Some(IdempotencyKey::new(key).expect("key")),
    }
}

fn input_request(session_id: &SessionId, text: &str) -> SubmitInputRequest {
    SubmitInputRequest {
        session_id: session_id.clone(),
        message: text.to_owned(),
        variant: AgentVariant::Build,
        mode: assistant_protocol::SubmitInputMode::Normal,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    }
}

async fn wait_for_command(
    runtime: &AssistantRuntime,
    session_id: &SessionId,
    input_id: &InputId,
) -> crate::StoredSessionCommand {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let session = runtime.session_for_test(session_id);
            let command = session
                .lock_state()
                .expect("state")
                .commands
                .get(input_id)
                .cloned()
                .expect("accepted command");
            if command.state == crate::StoredSessionCommandState::Committed {
                return command;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("command settles")
}

#[tokio::test]
async fn mcp_refresh_command_commits_without_run_and_next_model_reads_the_result() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "after-refresh-answer",
            "ready",
        )))],
    ));
    let runtime = with_mcp(runtime(model.clone()));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let request = refresh_request(&session_id, "refresh-once");
    let accepted = runtime
        .submit_session_command(request.clone())
        .await
        .expect("command")
        .accepted;
    let command = wait_for_command(&runtime, &session_id, &accepted.input_id).await;
    assert_eq!(
        command.result.as_ref().expect("result").outcome,
        McpRefreshOutcome::Success
    );
    assert!(model.take_requests().is_empty());
    assert!(
        runtime
            .session_for_test(&session_id)
            .run_snapshots()
            .expect("runs")
            .is_empty()
    );
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("view")
        .snapshot
        .value;
    assert!(view.queue.items.is_empty());
    assert!(matches!(
        view.conversation.items.as_slice(),
        [ConversationItem::ControlResult { .. }]
    ));
    assert_eq!(view.conversation_generation, 2);
    let duplicate = runtime
        .submit_session_command(request.clone())
        .await
        .expect("duplicate");
    assert!(duplicate.accepted.is_duplicate);
    assert_eq!(duplicate.accepted.input_id, accepted.input_id);
    let mut conflicting = request;
    conflicting.command = SessionCommand::McpRefresh {
        server: Some(McpServerKey::new("github").expect("server")),
    };
    assert!(runtime.submit_session_command(conflicting).await.is_err());
    let input = runtime
        .submit_input(input_request(&session_id, "continue"))
        .await
        .expect("input");
    wait_for_terminal(&runtime, &session_id, &input.run.run_id).await;
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    let conversation = serde_json::to_string(&requests[0].conversation).expect("JSON");
    assert!(conversation.contains("{RUNTIME_CONTROL_RESULT_V1}"));
    assert!(!conversation.contains("/mcp refresh"));
    let recovered = runtime.store.load_runtime().await.expect("stored facts");
    assert_eq!(recovered.session_commands.len(), 1);
    assert_eq!(recovered.runs.len(), 1);
    assert!(
        recovered
            .runs
            .iter()
            .all(|run| run.input_id != accepted.input_id)
    );
    let forked = runtime
        .fork_session(assistant_protocol::ForkSessionRequest {
            session_id: session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("after-refresh-answer")
                .expect("message"),
            expected_generation: 2,
        })
        .await
        .expect("fork control result history");
    let fork_view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: forked.session.session_id,
        })
        .await
        .expect("fork view")
        .snapshot
        .value;
    assert!(
        matches!(&fork_view.conversation.items[0], ConversationItem::ControlResult { message_id, .. }
        if message_id.as_str() != command.user_message_id.as_str())
    );
    assert!(fork_view.runs.is_empty());
}

#[tokio::test]
async fn mcp_refresh_command_does_not_bypass_pending_approval() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            mcp_tool_step("before-refresh-tool", "issue"),
            ModelScript::Events(message_events(&assistant_text(
                "before-refresh-final",
                "done",
            ))),
        ],
    ));
    let runtime = with_mcp(runtime(model));
    runtime.bootstrap_mcp().await.expect("bootstrap");
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let input = runtime
        .submit_input(input_request(&session_id, "create issue"))
        .await
        .expect("input");
    let approval = wait_for_pending_approval(&runtime, &session_id).await;
    let command = runtime
        .submit_session_command(refresh_request(&session_id, "blocked-refresh"))
        .await
        .expect("command")
        .accepted;
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("view")
        .snapshot
        .value;
    assert!(
        matches!(&view.queue.items[0], QueuedSessionItemSnapshot::Command(item)
        if item.input_id == command.input_id && item.state == SessionCommandQueueState::Queued)
    );
    assert_eq!(view.approvals.items.len(), 1);
    assert!(
        runtime
            .store
            .load_runtime()
            .await
            .expect("stored")
            .session_commands[0]
            .result
            .is_none()
    );
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: approval.approval_id,
            decision: assistant_protocol::ApprovalDecision::AllowOnce,
        })
        .await
        .expect("approve");
    wait_for_terminal(&runtime, &session_id, &input.run.run_id).await;
    wait_for_command(&runtime, &session_id, &command.input_id).await;
}

struct GatedMcpFactory {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl crate::McpConnectionFactory for GatedMcpFactory {
    fn connect(
        &self,
        _server: crate::McpServerConfig,
        _options: crate::McpConnectionOptions,
        _cancellation: CancellationToken,
    ) -> crate::McpConnectionFuture<'_, Arc<dyn crate::McpConnection>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(Arc::new(WorkingMcpTestConnection {
                calls: AtomicUsize::new(0),
            }) as Arc<dyn crate::McpConnection>)
        })
    }
}

#[tokio::test]
async fn executing_mcp_refresh_keeps_new_input_acceptance_live_but_blocks_consumption() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "after-gated-refresh",
            "done",
        )))],
    ));
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(GatedMcpFactory {
            entered: entered.clone(),
            release: release.clone(),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let command = runtime
        .submit_session_command(refresh_request(&session_id, "gated-refresh"))
        .await
        .expect("command")
        .accepted;
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("refresh starts");
    let input = tokio::time::timeout(
        Duration::from_secs(1),
        runtime.submit_input(input_request(&session_id, "queued behind refresh")),
    )
    .await
    .expect("acceptance is not blocked")
    .expect("input");
    let controller = runtime.session_for_test(&session_id);
    let queue =
        crate::runtime::product::queue_snapshot(&controller, &Default::default()).expect("queue");
    assert_eq!(queue.items.len(), 2);
    assert!(
        matches!(&queue.items[0], QueuedSessionItemSnapshot::Command(item) if item.state == SessionCommandQueueState::Executing)
    );
    assert_eq!(queue.items[1].input_id(), &input.input_id);
    assert!(model.take_requests().is_empty());
    let stored = runtime.store.load_runtime().await.expect("stored");
    assert_eq!(stored.session_commands.len(), 1);
    assert_eq!(stored.inputs.len(), 1);
    release.notify_one();
    wait_for_command(&runtime, &session_id, &command.input_id).await;
    wait_for_terminal(&runtime, &session_id, &input.run.run_id).await;
    assert_eq!(model.take_requests().len(), 1);
}

#[tokio::test]
async fn mixed_mcp_command_queue_recovers_paused_and_preserves_reordered_fifo() {
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("recovered-first", "one"))),
            ModelScript::Events(message_events(&assistant_text("recovered-second", "two"))),
        ],
    ));
    let first = with_mcp(
        runtime_with_store(
            model.clone(),
            store.clone(),
            RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        )
        .await,
    );
    let session_id = first
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    first
        .session_for_test(&session_id)
        .lock_state()
        .expect("state")
        .resume_required = true;
    let input_one = first
        .submit_input(input_request(&session_id, "first"))
        .await
        .expect("first");
    let command = first
        .submit_session_command(refresh_request(&session_id, "recover-refresh"))
        .await
        .expect("command")
        .accepted;
    let input_two = first
        .submit_input(input_request(&session_id, "second"))
        .await
        .expect("second");
    drop(first);
    let recovered = with_mcp(
        runtime_with_store(
            model.clone(),
            store,
            RuntimeConfig::new(NonZeroUsize::new(32).expect("capacity")),
        )
        .await,
    );
    let controller = recovered.session_for_test(&session_id);
    let queue =
        crate::runtime::product::queue_snapshot(&controller, &Default::default()).expect("queue");
    assert_eq!(
        queue.state,
        assistant_protocol::QueueExecutionState::ResumeRequired
    );
    assert_eq!(
        queue
            .items
            .iter()
            .map(|item| item.input_id())
            .collect::<Vec<_>>(),
        vec![&input_one.input_id, &command.input_id, &input_two.input_id]
    );
    let queue = recovered
        .prioritize_queued_input(assistant_protocol::PrioritizeQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: command.input_id.clone(),
            expected_revision: queue.revision,
        })
        .await
        .expect("prioritize command")
        .queue;
    assert_eq!(queue.items[0].input_id(), &command.input_id);
    recovered
        .resume_queued_input(ResumeQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: None,
            expected_revision: queue.revision,
        })
        .await
        .expect("resume");
    wait_for_command(&recovered, &session_id, &command.input_id).await;
    wait_for_terminal(&recovered, &session_id, &input_two.run.run_id).await;
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        serde_json::to_string(&requests[0].conversation)
            .expect("JSON")
            .contains("{RUNTIME_CONTROL_RESULT_V1}")
    );
}

struct BlockingCatalogConnection {
    entered: Notify,
    closed: AtomicUsize,
}

impl crate::McpConnection for BlockingCatalogConnection {
    fn list_tools_page(
        &self,
        _cursor: Option<String>,
        _cancellation: CancellationToken,
    ) -> crate::McpConnectionFuture<'_, crate::McpToolPage> {
        Box::pin(async {
            self.entered.notify_one();
            std::future::pending().await
        })
    }

    fn call_tool_once(
        &self,
        _name: String,
        _arguments: serde_json::Map<String, serde_json::Value>,
        _cancellation: CancellationToken,
    ) -> crate::McpConnectionFuture<'_, crate::McpRawCallResult> {
        Box::pin(std::future::pending())
    }

    fn close(&self, _cancellation: CancellationToken) -> crate::McpConnectionFuture<'_, ()> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }
}

struct TestClosingFactory {
    connection: Arc<dyn crate::McpConnection>,
    connects: AtomicUsize,
}

struct CatalogTestConnection {
    tools: Vec<crate::McpToolDefinition>,
    closed: AtomicUsize,
}

impl crate::McpConnection for CatalogTestConnection {
    fn list_tools_page(
        &self,
        _cursor: Option<String>,
        _cancellation: CancellationToken,
    ) -> crate::McpConnectionFuture<'_, crate::McpToolPage> {
        Box::pin(std::future::ready(Ok(crate::McpToolPage {
            tools: self.tools.clone(),
            next_cursor: None,
        })))
    }

    fn call_tool_once(
        &self,
        _name: String,
        _arguments: serde_json::Map<String, serde_json::Value>,
        _cancellation: CancellationToken,
    ) -> crate::McpConnectionFuture<'_, crate::McpRawCallResult> {
        panic!("connection tests must not execute tools");
    }

    fn close(&self, _cancellation: CancellationToken) -> crate::McpConnectionFuture<'_, ()> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }
}

#[tokio::test]
async fn mcp_connection_test_and_refresh_share_schema_duplicates_and_size_validation() {
    use assistant_protocol::{
        McpConnectionTestOutcome, McpDiagnosticCode, McpFieldChange, McpServerDraft,
        McpServerTransportDraft, TestMcpServerRequest,
    };
    let long = crate::McpToolDefinition {
        name: "design".to_owned(),
        title: None,
        description: Some("x".repeat(14_045)),
        input_schema: serde_json::json!({"type":"object"}),
        output_schema: None,
        annotations: None,
    };
    let invalid = crate::McpToolDefinition {
        input_schema: serde_json::json!({"$schema":"https://invalid.test/schema"}),
        ..long.clone()
    };
    let huge = crate::McpToolDefinition {
        description: Some("x".repeat(2 * 1024 * 1024)),
        ..long.clone()
    };
    let mut second = long.clone();
    second.name = "second_design".to_owned();
    for (tools, success, code) in [
        (
            vec![long.clone(), second],
            true,
            McpDiagnosticCode::ToolDescriptionLong,
        ),
        (vec![invalid], false, McpDiagnosticCode::SchemaInvalid),
        (
            vec![long.clone(), long],
            false,
            McpDiagnosticCode::CatalogFailed,
        ),
        (vec![huge], false, McpDiagnosticCode::LimitExceeded),
    ] {
        let connection = Arc::new(CatalogTestConnection {
            tools,
            closed: AtomicUsize::new(0),
        });
        let runtime = runtime(Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8192,
            Vec::<ModelScript>::new(),
        )))
        .with_mcp_services(
            Arc::new(StaticMcpTestSource),
            Arc::new(TestClosingFactory {
                connection: connection.clone(),
                connects: AtomicUsize::new(0),
            }),
            Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
        );
        let tested = runtime
            .test_mcp_server(TestMcpServerRequest {
                test_id: IdempotencyKey::new("catalog-test").expect("id"),
                server: McpServerDraft {
                    server_key: McpServerKey::new("github").expect("key"),
                    display_name: "GitHub".to_owned(),
                    description: String::new(),
                    enabled: true,
                    transport: McpServerTransportDraft::Stdio {
                        command: McpFieldChange::Keep,
                        args: McpFieldChange::Keep,
                        cwd: McpFieldChange::Keep,
                        environment: Default::default(),
                    },
                    startup_timeout_ms: None,
                    tool_timeout_ms: None,
                },
            })
            .await
            .expect("test result");
        assert_eq!(tested.outcome == McpConnectionTestOutcome::Success, success);
        assert_eq!(
            tested.diagnostic.as_ref().expect("warning or failure").code,
            code
        );
        assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
        assert!(
            runtime
                .mcp_service
                .registry
                .catalog_snapshot()
                .expect("registry unchanged")
                .is_empty()
        );
        let refreshed = runtime.bootstrap_mcp().await.expect("refresh result");
        assert_eq!(refreshed.outcome == McpRefreshOutcome::Success, success);
        assert_eq!(
            refreshed.servers[0]
                .diagnostic
                .as_ref()
                .expect("same diagnostic")
                .code,
            code
        );
        if success {
            assert_eq!(tested.tool_count, 2);
            let snapshot = runtime
                .get_mcp_configuration(assistant_protocol::GetMcpConfigurationRequest::default())
                .await
                .expect("snapshot")
                .snapshot;
            assert!(!snapshot.needs_refresh);
            assert_eq!(snapshot.servers[0].tool_count, 2);
            assert_eq!(
                snapshot.diagnostics.len(),
                2,
                "different tools' warnings must not be merged"
            );
        }
        runtime
            .mcp_service
            .registry
            .shutdown()
            .await
            .expect("shutdown");
        assert_eq!(connection.closed.load(Ordering::SeqCst), 2);
    }
}

impl crate::McpConnectionFactory for TestClosingFactory {
    fn connect(
        &self,
        _server: crate::McpServerConfig,
        _options: crate::McpConnectionOptions,
        _cancellation: CancellationToken,
    ) -> crate::McpConnectionFuture<'_, Arc<dyn crate::McpConnection>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(
            self.connection.clone() as Arc<dyn crate::McpConnection>
        )))
    }
}

#[tokio::test]
async fn mcp_management_test_cancellation_closes_candidate_and_handles_early_cancel() {
    use assistant_protocol::{
        CancelMcpServerTestRequest, McpFieldChange, McpServerDraft, McpServerTransportDraft,
        TestMcpServerRequest,
    };
    let connection = Arc::new(BlockingCatalogConnection {
        entered: Notify::new(),
        closed: AtomicUsize::new(0),
    });
    let factory = Arc::new(TestClosingFactory {
        connection: connection.clone(),
        connects: AtomicUsize::new(0),
    });
    let runtime = Arc::new(
        runtime(Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8192,
            Vec::<ModelScript>::new(),
        )))
        .with_mcp_services(
            Arc::new(StaticMcpTestSource),
            factory.clone(),
            Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
        ),
    );
    let request = TestMcpServerRequest {
        test_id: IdempotencyKey::new("cancel-current-test").expect("id"),
        server: McpServerDraft {
            server_key: McpServerKey::new("github").expect("key"),
            display_name: "GitHub".to_owned(),
            description: String::new(),
            enabled: true,
            transport: McpServerTransportDraft::Stdio {
                command: McpFieldChange::Keep,
                args: McpFieldChange::Keep,
                cwd: McpFieldChange::Keep,
                environment: Default::default(),
            },
            startup_timeout_ms: None,
            tool_timeout_ms: None,
        },
    };
    let testing = tokio::spawn({
        let runtime = runtime.clone();
        let request = request.clone();
        async move { runtime.test_mcp_server(request).await }
    });
    tokio::time::timeout(Duration::from_secs(1), connection.entered.notified())
        .await
        .expect("catalog entered");
    runtime
        .cancel_mcp_server_test(CancelMcpServerTestRequest {
            test_id: request.test_id.clone(),
        })
        .expect("cancel current");
    let result = tokio::time::timeout(Duration::from_secs(1), testing)
        .await
        .expect("bounded cancel")
        .expect("task")
        .expect("result");
    assert_eq!(
        result.outcome,
        assistant_protocol::McpConnectionTestOutcome::Cancelled
    );
    assert_eq!(connection.closed.load(Ordering::SeqCst), 1);
    assert!(
        runtime
            .mcp_service
            .registry
            .catalog_snapshot()
            .expect("no active registry")
            .is_empty()
    );
    let early = IdempotencyKey::new("cancel-before-test").expect("id");
    runtime
        .cancel_mcp_server_test(CancelMcpServerTestRequest {
            test_id: early.clone(),
        })
        .expect("early cancel");
    let result = runtime
        .test_mcp_server(TestMcpServerRequest {
            test_id: early,
            ..request
        })
        .await
        .expect("already cancelled");
    assert_eq!(
        result.outcome,
        assistant_protocol::McpConnectionTestOutcome::Cancelled
    );
    assert_eq!(factory.connects.load(Ordering::SeqCst), 1);
}
