use super::*;

use assistant_protocol::{
    ConversationItem, ConversationOwner, GetApplicationSnapshotRequest,
    GetConversationPageAroundRunRequest, GetSessionViewRequest, GetToolDetailRequest,
    InterruptRunRequest, ListConversationPageRequest, MessageFeedback,
    PrioritizeQueuedInputRequest, QueueExecutionState, ReenterFromUserMessageRequest,
    SetMessageFeedbackRequest,
};

use crate::{
    McpConfigSource, McpConnection, McpConnectionError, McpConnectionFactory,
    McpConnectionFailureKind, McpConnectionFuture, McpConnectionOptions, McpRawCallResult,
    McpRawContent, McpServerConfig, McpToolDefinition, McpToolPage,
    runtime::product::{empty_child_projection, project_conversation},
};

struct MissingMcpTestSource;

impl McpConfigSource for MissingMcpTestSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(std::future::ready(ConfigSourceLoad::Missing))
    }

    fn replace(
        &self,
        _expected_revision: Option<String>,
        _document: String,
    ) -> ConfigSourceReplaceFuture<'_> {
        Box::pin(std::future::ready(ConfigSourceReplace::Unavailable(
            ConfigSourceFailure::new(ConfigSourceFailureKind::Read, "test source is read-only"),
        )))
    }
}

struct FailingMcpTestFactory;

impl McpConnectionFactory for FailingMcpTestFactory {
    fn connect(
        &self,
        _server: McpServerConfig,
        _options: McpConnectionOptions,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> McpConnectionFuture<'_, Arc<dyn McpConnection>> {
        Box::pin(std::future::ready(Err(McpConnectionError::new(
            McpConnectionFailureKind::Connect,
            "test connection is unavailable",
        ))))
    }
}

struct PendingMcpTestFactory;

pub(super) struct StaticMcpTestSource;

pub(super) struct WorkingMcpTestConnection {
    pub(super) calls: AtomicUsize,
}

pub(super) struct WorkingMcpTestFactory {
    pub(super) connection: Arc<WorkingMcpTestConnection>,
}

pub(super) fn mcp_tool_step(message_id: &str, title: &str) -> ModelScript {
    mcp_tool_step_with_arguments(message_id, json!({"title": title}))
}

fn mcp_tool_step_with_arguments(message_id: &str, arguments: serde_json::Value) -> ModelScript {
    ModelScript::Events(message_events(&AssistantMessage {
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(format!("{message_id}-call")).expect("tool call id"),
            name: ToolName::new("call_mcp_tool").expect("gateway name"),
            arguments: json!({
                "server": "github",
                "tool": "create_issue",
                "arguments": arguments
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }))
}

fn discover_mcp_step(message_id: &str, arguments: serde_json::Value) -> ModelScript {
    ModelScript::Events(message_events(&AssistantMessage {
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new(format!("{message_id}-call")).expect("tool call id"),
            name: ToolName::new("discover_mcp_tools").expect("discovery name"),
            arguments,
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }))
}

impl McpConfigSource for StaticMcpTestSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(std::future::ready(ConfigSourceLoad::Document(
            ConfigDocument::new(
                r#"{"mcpServers":{"github":{"command":"fixture","displayName":"GitHub","description":"Issue operations"}}}"#.to_owned(),
                "mcp-test-revision".to_owned(),
            ),
        )))
    }

    fn replace(
        &self,
        _expected_revision: Option<String>,
        _document: String,
    ) -> ConfigSourceReplaceFuture<'_> {
        Box::pin(std::future::ready(ConfigSourceReplace::Unavailable(
            ConfigSourceFailure::new(ConfigSourceFailureKind::Read, "test source is read-only"),
        )))
    }
}

impl McpConnection for WorkingMcpTestConnection {
    fn list_tools_page(
        &self,
        _cursor: Option<String>,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> McpConnectionFuture<'_, McpToolPage> {
        Box::pin(std::future::ready(Ok(McpToolPage {
            tools: vec![
                McpToolDefinition {
                    name: "create_issue".to_owned(),
                    title: Some("Create issue".to_owned()),
                    description: Some("Create one issue".to_owned()),
                    input_schema: json!({
                        "type": "object",
                        "required": ["title"],
                        "properties": {"title": {"type": "string"}}
                    }),
                    output_schema: None,
                    annotations: Some(json!({"destructiveHint": true})),
                },
                McpToolDefinition {
                    name: "list_issues".to_owned(),
                    title: Some("List issues".to_owned()),
                    description: Some("Read repository issues".to_owned()),
                    input_schema: json!({
                        "type": "object",
                        "properties": {"state": {"type": "string"}}
                    }),
                    output_schema: None,
                    annotations: Some(json!({"readOnlyHint": true})),
                },
            ],
            next_cursor: None,
        })))
    }

    fn call_tool_once(
        &self,
        tool_name: String,
        arguments: serde_json::Map<String, serde_json::Value>,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> McpConnectionFuture<'_, McpRawCallResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(McpRawCallResult {
            content: vec![McpRawContent::Text {
                text: format!(
                    "called {tool_name}:{}",
                    arguments
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                ),
            }],
            structured_content: None,
            is_error: false,
        })))
    }

    fn close(
        &self,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> McpConnectionFuture<'_, ()> {
        Box::pin(std::future::ready(Ok(())))
    }
}

impl McpConnectionFactory for WorkingMcpTestFactory {
    fn connect(
        &self,
        _server: McpServerConfig,
        _options: McpConnectionOptions,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> McpConnectionFuture<'_, Arc<dyn McpConnection>> {
        Box::pin(std::future::ready(Ok(
            self.connection.clone() as Arc<dyn McpConnection>
        )))
    }
}

impl McpConnectionFactory for PendingMcpTestFactory {
    fn connect(
        &self,
        _server: McpServerConfig,
        _options: McpConnectionOptions,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> McpConnectionFuture<'_, Arc<dyn McpConnection>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn markdown_export_contains_product_content_without_runtime_metadata() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "export-answer",
            "exported answer",
        )))],
    )));
    let session = runtime
        .create_session(CreateSessionRequest {
            title: Some("Export title".to_owned()),
            ..CreateSessionRequest::default()
        })
        .await
        .expect("session");
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session.session.session_id.clone(),
            message: "exported question".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("submit");
    wait_for_terminal(&runtime, &session.session.session_id, &submitted.run.run_id).await;

    let controller = runtime
        .session(&session.session.session_id)
        .expect("session controller");
    controller
        .lock_state()
        .expect("session state")
        .journal
        .as_mut()
        .expect("conversation journal")
        .append_completed(ConversationMessage::User(agent_types::UserMessage {
            id: MessageId::new("runtime-hidden-export").expect("message id"),
            origin: agent_types::UserMessageOrigin::Runtime,
            transcript_visibility: agent_types::TranscriptVisibility::Hidden,
            parts: vec![UserPart::Injected(TextPart {
                id: PartId::new("runtime-hidden-export-injected").expect("part id"),
                text: "runtime-export-secret".to_owned(),
            })],
        }))
        .expect("append hidden runtime message");

    let markdown = runtime
        .export_session_markdown(&session.session.session_id)
        .await
        .expect("export");
    assert!(markdown.starts_with("# Export title\n\n"));
    assert!(markdown.contains("## 用户\n\nexported question"));
    assert!(markdown.contains("## 助手\n\nexported answer"));
    assert!(!markdown.contains("provider_state"));
    assert!(!markdown.contains("agent_readable_path"));
    assert!(!markdown.contains("runtime-export-secret"));
}

#[test]
fn conversation_projection_omits_hidden_runtime_user_messages() {
    let snapshot = agent_types::ConversationSnapshot::new(vec![
        ConversationMessage::User(agent_types::UserMessage {
            id: MessageId::new("visible-user").expect("message id"),
            origin: agent_types::UserMessageOrigin::User,
            transcript_visibility: agent_types::TranscriptVisibility::Visible,
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new("visible-user-text").expect("part id"),
                text: "visible question".to_owned(),
            })],
        }),
        ConversationMessage::User(agent_types::UserMessage {
            id: MessageId::new("runtime-hidden-user").expect("message id"),
            origin: agent_types::UserMessageOrigin::Runtime,
            transcript_visibility: agent_types::TranscriptVisibility::Hidden,
            parts: vec![UserPart::Injected(TextPart {
                id: PartId::new("runtime-hidden-user-injected").expect("part id"),
                text: "continue internally".to_owned(),
            })],
        }),
    ]);

    let items = project_conversation(&snapshot, &empty_child_projection()).expect("projection");
    assert_eq!(items.len(), 1);
    let ConversationItem::User(user) = &items[0] else {
        panic!("visible user item")
    };
    assert_eq!(user.message_id.as_str(), "visible-user");
    assert_eq!(user.text, "visible question");
}

#[tokio::test]
async fn completed_assistant_turn_exposes_the_reliable_run_finish_time() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "finished-answer",
            "done",
        )))],
    )));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "finish time".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("input")
        .run;
    wait_for_terminal(&runtime, &session_id, &run.run_id).await;

    let message = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: ConversationOwner::MainSession { session_id },
            cursor: None,
            limit: 20,
        })
        .await
        .expect("conversation")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) => Some(message),
            ConversationItem::User(_)
            | ConversationItem::ControlResult { .. }
            | ConversationItem::ContextSummary { .. } => None,
        })
        .expect("assistant message");

    assert_eq!(
        message.status,
        Some(assistant_protocol::RunStatus::Completed)
    );
    assert!(message.finished_at_ms.is_some());
}

#[tokio::test]
async fn session_usage_projects_latest_and_token_weighted_cache_hit_rates() {
    let mut first = assistant_text("cache-rate-first", "first");
    first.usage = Some(agent_types::TokenUsage {
        input_tokens: 100,
        output_tokens: 10,
        total_tokens: 110,
        cached_input_tokens: Some(20),
        reasoning_tokens: None,
    });
    let mut second = assistant_text("cache-rate-second", "second");
    second.usage = Some(agent_types::TokenUsage {
        input_tokens: 300,
        output_tokens: 20,
        total_tokens: 320,
        cached_input_tokens: Some(150),
        reasoning_tokens: None,
    });
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&first)),
            ModelScript::Events(message_events(&second)),
        ],
    )));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    for message in ["first request", "second request"] {
        let run = runtime
            .submit_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                session_id: session_id.clone(),
                message: message.to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
                variant: assistant_protocol::AgentVariant::Build,
            })
            .await
            .expect("submit")
            .run;
        wait_for_terminal(&runtime, &session_id, &run.run_id).await;
    }

    let usage = runtime
        .get_session_view(GetSessionViewRequest { session_id })
        .await
        .expect("session view")
        .snapshot
        .value
        .usage;
    assert_eq!(usage.latest_cache_hit_basis_points, Some(5_000));
    assert_eq!(usage.overall_cache_hit_basis_points, Some(4_250));
}

#[tokio::test]
async fn assistant_feedback_is_persisted_in_the_conversation_projection_and_can_be_cleared() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "feedback-answer",
            "answer",
        )))],
    )));
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "question".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("input")
        .run;
    wait_for_terminal(&runtime, &session_id, &run.run_id).await;
    let owner = ConversationOwner::MainSession {
        session_id: session_id.clone(),
    };
    let message_id = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: owner.clone(),
            cursor: None,
            limit: 20,
        })
        .await
        .expect("page")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) => Some(message.message_id),
            ConversationItem::User(_)
            | ConversationItem::ControlResult { .. }
            | ConversationItem::ContextSummary { .. } => None,
        })
        .expect("assistant message");
    runtime
        .set_message_feedback(SetMessageFeedbackRequest {
            session_id: session_id.clone(),
            message_id: message_id.clone(),
            feedback: Some(MessageFeedback::Positive),
        })
        .await
        .expect("feedback");
    let feedback = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: owner.clone(),
            cursor: None,
            limit: 20,
        })
        .await
        .expect("feedback page")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) if message.message_id == message_id => {
                Some(message.feedback)
            }
            _ => None,
        });
    assert_eq!(feedback, Some(Some(MessageFeedback::Positive)));
    runtime
        .set_message_feedback(SetMessageFeedbackRequest {
            session_id,
            message_id,
            feedback: None,
        })
        .await
        .expect("clear feedback");
    let cleared = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner,
            cursor: None,
            limit: 20,
        })
        .await
        .expect("cleared page")
        .snapshot
        .value
        .items
        .into_iter()
        .find_map(|item| match item {
            ConversationItem::Assistant(message) => Some(message.feedback),
            ConversationItem::User(_)
            | ConversationItem::ControlResult { .. }
            | ConversationItem::ContextSummary { .. } => None,
        });
    assert_eq!(cleared, Some(None));
}

#[tokio::test]
async fn product_event_envelopes_and_application_snapshot_share_a_waterline() {
    let runtime = runtime_with_tools(
        Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            Vec::<ModelScript>::new(),
        )),
        ToolSetSnapshot::default(),
    );
    let mut events = runtime.subscribe_event_envelopes();
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let event = events.recv().await.expect("session event");
    assert_eq!(event.sequence, 1);
    assert!(matches!(
        event.event,
        RuntimeEvent::SessionCreated { session }
            if session.session_id == created.session.session_id
    ));

    let snapshot = runtime
        .get_application_snapshot(GetApplicationSnapshotRequest::default())
        .await
        .expect("application snapshot")
        .snapshot;
    assert_eq!(snapshot.observed_sequence, 1);
    assert_eq!(snapshot.value.active_sessions.len(), 1);
    assert!(snapshot.value.capabilities.conversation_paging);
}

#[tokio::test]
async fn installed_mcp_management_is_advertised_without_enabling_run_tools() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        Vec::<ModelScript>::new(),
    )))
    .with_mcp_services(
        Arc::new(MissingMcpTestSource),
        Arc::new(FailingMcpTestFactory),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    let snapshot = runtime
        .get_application_snapshot(GetApplicationSnapshotRequest::default())
        .await
        .expect("application snapshot")
        .snapshot
        .value;
    assert!(snapshot.capabilities.mcp_management);
    assert!(!snapshot.capabilities.mcp_tools);
    assert!(snapshot.capabilities.session_commands);

    let configuration = runtime
        .get_mcp_configuration(assistant_protocol::GetMcpConfigurationRequest::default())
        .await
        .expect("empty MCP configuration");
    assert_eq!(configuration.snapshot.revision, "absent");
    assert!(configuration.snapshot.servers.is_empty());
}

#[tokio::test]
async fn mcp_discovery_is_hierarchical_filtered_and_request_only() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            discover_mcp_step(
                "mcp-discovery",
                json!({"server": "github", "detail": "full"}),
            ),
            ModelScript::Events(message_events(&assistant_text(
                "mcp-discovery-final",
                "selected create_issue",
            ))),
        ],
    ));
    let connection = Arc::new(WorkingMcpTestConnection {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory { connection }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.expect("bootstrap MCP");
    let session_id = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let scope = PermissionFileScope::Session(session_id.clone());
    let loaded = runtime
        .permission_coordinator
        .load_document(scope.clone())
        .await
        .expect("load permissions");
    runtime
        .permission_coordinator
        .replace_document(
            scope,
            loaded.revision,
            crate::PermissionDocument {
                schema_version: 2,
                rules: vec![crate::PermissionRule {
                    id: "deny-list-issues".to_owned(),
                    effect: crate::PermissionEffect::Deny,
                    variants: vec![assistant_protocol::AgentVariant::Plan],
                    matcher: crate::PermissionMatcher::Mcp(crate::McpPermissionMatcher {
                        server: crate::McpPermissionServerMatch::Exact {
                            value: assistant_protocol::McpServerKey::new("github").expect("server"),
                        },
                        tool: crate::McpPermissionToolMatch::Exact {
                            value: "list_issues".to_owned(),
                        },
                    }),
                }],
            },
        )
        .await
        .expect("persist deny");
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "find the right GitHub tool".to_owned(),
            variant: assistant_protocol::AgentVariant::Plan,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit discovery input");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let names = request
            .tools
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "discover_mcp_tools")
                .count(),
            1
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "call_mcp_tool")
                .count(),
            1
        );
        assert!(!names.contains(&"create_issue"));
        assert!(!names.contains(&"list_issues"));
    }
    let first_request = serde_json::to_string(&requests[0].conversation).expect("request JSON");
    assert!(first_request.contains("MCP_SERVER_DIRECTORY_V1"));
    assert!(first_request.contains("github"));
    assert!(!first_request.contains("create_issue"));
    let second_request = serde_json::to_string(&requests[1].conversation).expect("request JSON");
    assert!(second_request.contains("create_issue"));
    assert!(second_request.contains("input_schema"));
    assert!(!second_request.contains("list_issues"));
    let persisted = runtime
        .store
        .load_conversation(&session_id)
        .await
        .expect("persisted conversation");
    assert!(
        !serde_json::to_string(&persisted)
            .expect("persisted JSON")
            .contains("MCP_SERVER_DIRECTORY_V1")
    );
}

#[tokio::test]
async fn selected_mcp_freezes_queue_conversation_fork_and_full_run_disclosure() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "selected-mcp-answer",
            "done",
        )))],
    ));
    let connection = Arc::new(WorkingMcpTestConnection {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory { connection }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.expect("bootstrap MCP");
    let session_id = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let options = runtime
        .list_mcp_server_options(assistant_protocol::ListMcpServerOptionsRequest {
            context: assistant_protocol::McpServerOptionsContext::Session {
                session_id: session_id.clone(),
            },
            variant: assistant_protocol::AgentVariant::Build,
        })
        .expect("MCP options");
    assert_eq!(options.servers.len(), 1);
    assert_eq!(options.servers[0].server_key.as_str(), "github");
    assert_eq!(options.servers[0].visible_tool_count, 2);
    let controller = runtime.session_for_test(&session_id);
    controller.lock_state().expect("state").queue_paused_by_user = true;
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "use GitHub".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: Some(assistant_protocol::McpServerKey::new("github").expect("server")),
            idempotency_key: None,
        })
        .await
        .expect("submit selected MCP input");
    let queue = crate::runtime::product::queue_snapshot(&controller, &Default::default())
        .expect("queue snapshot");
    assert_eq!(
        queue.items[0]
            .as_message()
            .expect("message")
            .mcp_selection
            .as_ref()
            .map(|tag| tag.server_key.as_str()),
        Some("github")
    );
    runtime
        .resume_queued_input(assistant_protocol::ResumeQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: None,
            expected_revision: queue.revision,
        })
        .await
        .expect("resume selected input");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    let request_json = serde_json::to_string(&requests[0].conversation).expect("request JSON");
    assert!(request_json.contains("MCP_SERVER_SELECTION_V1"));
    assert!(request_json.contains("disclosure"));
    assert!(request_json.contains("full"));
    assert!(request_json.contains("create_issue"));
    assert!(request_json.contains("list_issues"));

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
            if user.mcp_selection.as_ref().is_some_and(|tag| tag.server_key.as_str() == "github")
    ));
    let forked = runtime
        .fork_session(assistant_protocol::ForkSessionRequest {
            session_id,
            fork_point: assistant_protocol::MessageId::new("selected-mcp-answer")
                .expect("message id"),
            expected_generation: view.conversation.generation,
        })
        .await
        .expect("fork selected MCP prefix");
    let fork_view = runtime
        .get_session_view(assistant_protocol::GetSessionViewRequest {
            session_id: forked.session.session_id,
        })
        .await
        .expect("fork view")
        .snapshot
        .value;
    assert!(matches!(
        &fork_view.conversation.items[0],
        assistant_protocol::ConversationItem::User(user)
            if user.mcp_selection.as_ref().is_some_and(|tag| tag.server_key.as_str() == "github")
    ));
}

#[tokio::test]
async fn selected_mcp_is_inherited_by_goal_runs_but_not_the_next_ordinary_input() {
    let completion_signal = AssistantMessage {
        id: MessageId::new("selected-goal-signal").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("selected-goal-signal-call").expect("call id"),
            name: ToolName::new("update_goal").expect("tool name"),
            arguments: json!({"status": "complete", "summary": "done"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "selected-goal-first",
                "continuing",
            ))),
            ModelScript::Events(message_events(&completion_signal)),
            ModelScript::Events(message_events(&assistant_text(
                "selected-goal-final",
                "completed",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "ordinary-after-selected-goal",
                "ordinary",
            ))),
        ],
    ));
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory {
            connection: Arc::new(WorkingMcpTestConnection {
                calls: AtomicUsize::new(0),
            }),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.expect("bootstrap MCP");
    let session_id = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let goal = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::StartGoal,
            session_id: session_id.clone(),
            message: "finish with GitHub".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: Some(assistant_protocol::McpServerKey::new("github").expect("server")),
            idempotency_key: Some(
                assistant_protocol::IdempotencyKey::new("selected-goal").expect("idempotency key"),
            ),
        })
        .await
        .expect("start selected Goal");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &goal.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime
                .session(&session_id)
                .expect("session")
                .lock_state()
                .expect("state")
                .goal
                .is_none()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Goal continuation completes");
    let ordinary = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "ordinary next turn".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit ordinary input");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &ordinary.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 4);
    for request in &requests[..3] {
        let request = serde_json::to_string(&request.conversation).expect("request JSON");
        assert!(request.contains("MCP_SERVER_SELECTION_V1"));
    }
    let ordinary = serde_json::to_string(&requests[3].conversation).expect("ordinary JSON");
    assert!(ordinary.contains("MCP_SERVER_DIRECTORY_V1"));
    assert!(!ordinary.contains("MCP_SERVER_SELECTION_V1"));
}

#[tokio::test]
async fn selected_mcp_disclosure_and_fixed_gateways_are_inherited_by_child_agent() {
    let delegate = AssistantMessage {
        id: MessageId::new("selected-mcp-delegate").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider id"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("selected-mcp-delegate-call").expect("call id"),
            name: ToolName::new("delegate_task").expect("tool name"),
            arguments: json!({
                "title": "Inspect GitHub",
                "task": "Inspect the selected server.",
                "context": "Use only disclosed capabilities.",
                "expected_output": "One sentence."
            }),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&delegate)),
            ModelScript::Events(message_events(&assistant_text(
                "selected-mcp-child-final",
                "child done",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "selected-mcp-parent-final",
                "parent done",
            ))),
        ],
    ));
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory {
            connection: Arc::new(WorkingMcpTestConnection {
                calls: AtomicUsize::new(0),
            }),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.expect("bootstrap MCP");
    let session_id = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    set_auto_approval(&runtime, &session_id).await;
    let submitted = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "delegate with selected GitHub".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: Some(assistant_protocol::McpServerKey::new("github").expect("server")),
            idempotency_key: None,
        })
        .await
        .expect("submit selected delegation");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &submitted.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        let names = request
            .tools
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"discover_mcp_tools"));
        assert!(names.contains(&"call_mcp_tool"));
        assert!(
            serde_json::to_string(&request.conversation)
                .expect("request JSON")
                .contains("MCP_SERVER_SELECTION_V1")
        );
    }
    assert!(
        requests[1]
            .tools
            .iter()
            .all(|definition| definition.name.as_str() != "delegate_task")
    );
}

#[tokio::test]
async fn mcp_gateway_uses_real_identity_approval_schema_two_and_history_projection() {
    let connection = Arc::new(WorkingMcpTestConnection {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            mcp_tool_step("mcp-tool-1", "first"),
            ModelScript::Events(message_events(&assistant_text("mcp-final-1", "done"))),
            mcp_tool_step("mcp-tool-2", "second"),
            ModelScript::Events(message_events(&assistant_text("mcp-final-2", "done"))),
        ],
    )))
    .with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory {
            connection: connection.clone(),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.expect("bootstrap MCP");
    let mut events = runtime.subscribe_events();
    assert!(
        runtime
            .get_application_snapshot(GetApplicationSnapshotRequest::default())
            .await
            .expect("application snapshot")
            .snapshot
            .value
            .capabilities
            .mcp_tools
    );
    let session_id = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let first = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "create the first issue".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit MCP input")
        .run;
    let approval = wait_for_pending_approval(&runtime, &session_id).await;
    let assistant_protocol::ToolApprovalSubject::Mcp {
        identity,
        arguments_json,
        untrusted_annotations_json,
    } = &approval.subject
    else {
        panic!("MCP approval subject");
    };
    assert_eq!(identity.server_key.as_str(), "github");
    assert_eq!(identity.server_display_name, "GitHub");
    assert_eq!(identity.tool_name, "create_issue");
    assert!(arguments_json.contains("first"));
    assert!(
        untrusted_annotations_json
            .as_deref()
            .is_some_and(|value| value.contains("destructiveHint"))
    );
    let approved_identity = identity.clone();
    let first_approval_id = approval.approval_id.clone();
    assert_eq!(connection.calls.load(Ordering::SeqCst), 0);

    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: approval.approval_id.clone(),
            decision: assistant_protocol::ApprovalDecision::AllowSession,
        })
        .await
        .expect("persist MCP approval");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &first.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let permission = runtime
        .permission_coordinator
        .registry()
        .snapshot(&PermissionFileScope::Session(session_id.clone()))
        .expect("permission registry")
        .expect("session permissions");
    let document = permission.document.as_ref().expect("valid permissions");
    assert_eq!(document.schema_version, 2);
    assert!(matches!(
        &document.rules[0].matcher,
        crate::PermissionMatcher::Mcp(crate::McpPermissionMatcher {
            server: crate::McpPermissionServerMatch::Exact { value },
            tool: crate::McpPermissionToolMatch::Exact { value: tool },
        }) if value.as_str() == "github" && tool == "create_issue"
    ));

    let detail = runtime
        .get_tool_detail(GetToolDetailRequest {
            owner: ConversationOwner::MainSession {
                session_id: session_id.clone(),
            },
            message_id: assistant_protocol::MessageId::new("mcp-tool-1").expect("message id"),
            call_id: assistant_protocol::ToolCallId::new("mcp-tool-1-call").expect("call id"),
        })
        .await
        .expect("MCP tool detail")
        .snapshot
        .value;
    assert_eq!(detail.mcp_identity, Some(approved_identity));
    assert!(matches!(
        detail.input,
        assistant_protocol::ToolInputSnapshot::Mcp { .. }
    ));

    runtime
        .set_session_approval_mode(SetSessionApprovalModeRequest {
            session_id: session_id.clone(),
            approval_mode: assistant_protocol::ApprovalMode::Auto,
        })
        .await
        .expect("set Auto");
    let second = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "create the second issue".to_owned(),
            variant: assistant_protocol::AgentVariant::Plan,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit Plan MCP input")
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &second.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(connection.calls.load(Ordering::SeqCst), 2);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest { session_id })
            .expect("pending approvals")
            .approvals
            .is_empty()
    );
    let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    let approval_resolved = events
        .iter()
        .position(|event| {
            matches!(
                event,
                RuntimeEvent::ApprovalResolved { approval_id, .. }
                    if approval_id == &first_approval_id
            )
        })
        .expect("MCP approval resolved event");
    let tool_started = events
        .iter()
        .position(|event| {
            matches!(
                event,
                RuntimeEvent::ToolStarted { run_id, call_id, .. }
                    if run_id == &first.run_id && call_id.as_str() == "mcp-tool-1-call"
            )
        })
        .expect("MCP tool started event");
    let tool_completed = events
        .iter()
        .position(|event| {
            matches!(
                event,
                RuntimeEvent::ToolCompleted { run_id, call_id, .. }
                    if run_id == &first.run_id && call_id.as_str() == "mcp-tool-1-call"
            )
        })
        .expect("MCP tool completed event");
    assert!(approval_resolved < tool_started && tool_started < tool_completed);
}

#[tokio::test]
async fn invalid_mcp_arguments_and_explicit_deny_never_reach_the_remote_server() {
    let connection = Arc::new(WorkingMcpTestConnection {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            mcp_tool_step_with_arguments("mcp-invalid", json!({"title": 1})),
            ModelScript::Events(message_events(&assistant_text(
                "mcp-invalid-final",
                "fixed",
            ))),
            mcp_tool_step("mcp-denied", "blocked"),
            ModelScript::Events(message_events(&assistant_text(
                "mcp-denied-final",
                "denied",
            ))),
        ],
    )))
    .with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory {
            connection: connection.clone(),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.expect("bootstrap MCP");
    let session_id = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let invalid = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "invalid arguments".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit invalid input")
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &invalid.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(connection.calls.load(Ordering::SeqCst), 0);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest {
                session_id: session_id.clone(),
            })
            .expect("pending approvals")
            .approvals
            .is_empty()
    );

    let scope = PermissionFileScope::Session(session_id.clone());
    let loaded = runtime
        .permission_coordinator
        .load_document(scope.clone())
        .await
        .expect("load permissions");
    runtime
        .permission_coordinator
        .replace_document(
            scope,
            loaded.revision,
            crate::PermissionDocument {
                schema_version: 2,
                rules: vec![crate::PermissionRule {
                    id: "deny-mcp".to_owned(),
                    effect: crate::PermissionEffect::Deny,
                    variants: vec![assistant_protocol::AgentVariant::Plan],
                    matcher: crate::PermissionMatcher::Mcp(crate::McpPermissionMatcher {
                        server: crate::McpPermissionServerMatch::Exact {
                            value: assistant_protocol::McpServerKey::new("github").expect("server"),
                        },
                        tool: crate::McpPermissionToolMatch::Exact {
                            value: "create_issue".to_owned(),
                        },
                    }),
                }],
            },
        )
        .await
        .expect("persist deny");
    runtime
        .set_session_approval_mode(SetSessionApprovalModeRequest {
            session_id: session_id.clone(),
            approval_mode: assistant_protocol::ApprovalMode::Auto,
        })
        .await
        .expect("set Auto");
    let denied = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "denied call".to_owned(),
            variant: assistant_protocol::AgentVariant::Plan,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit denied input")
        .run;
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &denied.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(connection.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refreshing_a_server_cancels_its_pending_approval_without_calling_remote() {
    let connection = Arc::new(WorkingMcpTestConnection {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            mcp_tool_step("mcp-refresh", "stale"),
            ModelScript::Events(message_events(&assistant_text(
                "mcp-refresh-final",
                "catalog changed",
            ))),
        ],
    )))
    .with_mcp_services(
        Arc::new(StaticMcpTestSource),
        Arc::new(WorkingMcpTestFactory {
            connection: connection.clone(),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.expect("bootstrap MCP");
    let mut events = runtime.subscribe_events();
    let session_id = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .expect("create session")
        .session
        .session_id;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "call before refresh".to_owned(),
            variant: assistant_protocol::AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("submit MCP input")
        .run;
    let approval = wait_for_pending_approval(&runtime, &session_id).await;
    assert_eq!(connection.calls.load(Ordering::SeqCst), 0);

    runtime.bootstrap_mcp().await.expect("refresh MCP");

    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(connection.calls.load(Ordering::SeqCst), 0);
    assert!(
        runtime
            .list_pending_approvals(ListPendingApprovalsRequest { session_id })
            .expect("pending approvals")
            .approvals
            .is_empty()
    );
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(
            event,
            RuntimeEvent::ApprovalCancelled { approval_id, .. }
                if approval_id == approval.approval_id
        )
    }));
}

#[tokio::test]
async fn mcp_candidate_test_times_out_without_persisting_or_enabling_tools() {
    let runtime = runtime(Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        Vec::<ModelScript>::new(),
    )))
    .with_mcp_services(
        Arc::new(MissingMcpTestSource),
        Arc::new(PendingMcpTestFactory),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.config_registry.replace_document_for_test(&format!(
        "{TEST_CONFIG}\n[mcp]\nconnect_timeout_ms = 1000\ncatalog_timeout_ms = 1000\nrequest_timeout_ms = 1000\nclose_timeout_ms = 1000\nmax_concurrent_calls_per_server = 1\n"
    ));
    let result = runtime
        .test_mcp_server(assistant_protocol::TestMcpServerRequest {
            test_id: assistant_protocol::IdempotencyKey::new("timeout-test").expect("test id"),
            server: assistant_protocol::McpServerDraft {
                server_key: assistant_protocol::McpServerKey::new("timeout").expect("server key"),
                display_name: "Timeout".to_owned(),
                description: "timeout fixture".to_owned(),
                enabled: true,
                transport: assistant_protocol::McpServerTransportDraft::Stdio {
                    command: assistant_protocol::McpFieldChange::Replace("fixture".to_owned()),
                    args: assistant_protocol::McpFieldChange::Replace(Vec::new()),
                    cwd: assistant_protocol::McpFieldChange::Remove,
                    environment: std::collections::BTreeMap::new(),
                },
                startup_timeout_ms: None,
                tool_timeout_ms: None,
            },
        })
        .await
        .expect("bounded candidate test result");
    assert_eq!(
        result.outcome,
        assistant_protocol::McpConnectionTestOutcome::Failure
    );
    assert_eq!(
        result.stage,
        assistant_protocol::McpConnectionTestStage::Connect
    );
    let configuration = runtime
        .get_mcp_configuration(assistant_protocol::GetMcpConfigurationRequest::default())
        .await
        .expect("MCP configuration after test");
    assert!(configuration.snapshot.servers.is_empty());
}

#[tokio::test]
async fn conversation_pages_are_latest_first_queries_with_generation_bound_cursors() {
    let runtime = runtime_with_tools(
        Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8_192,
            [
                ModelScript::Events(message_events(&assistant_text("a-page-1", "first"))),
                ModelScript::Events(message_events(&assistant_text("a-page-2", "second"))),
            ],
        )),
        ToolSetSnapshot::default(),
    );
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    for message in ["one", "two"] {
        let submitted = runtime
            .submit_input(SubmitInputRequest {
                mode: assistant_protocol::SubmitInputMode::Normal,
                session_id: session.session.session_id.clone(),
                message: message.to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
                variant: assistant_protocol::AgentVariant::Build,
            })
            .await
            .expect("submit");
        wait_for_terminal(&runtime, &session.session.session_id, &submitted.run.run_id).await;
    }

    let owner = ConversationOwner::MainSession {
        session_id: session.session.session_id.clone(),
    };
    let latest = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: owner.clone(),
            cursor: None,
            limit: 2,
        })
        .await
        .expect("latest page")
        .snapshot
        .value;
    assert!(latest.has_more);
    assert!(matches!(
        &latest.items[0],
        ConversationItem::User(message) if message.text == "two"
    ));
    assert!(matches!(
        &latest.items[1],
        ConversationItem::Assistant(message)
            if matches!(&message.segments[0], assistant_protocol::AssistantSegment::Text { text, .. } if text == "second")
    ));

    let old_cursor = latest.previous_cursor.clone().expect("older cursor");
    let old_generation = latest.generation;
    let older = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner,
            cursor: Some(old_cursor.clone()),
            limit: 2,
        })
        .await
        .expect("older page")
        .snapshot
        .value;
    assert!(!older.has_more);
    assert!(matches!(
        &older.items[0],
        ConversationItem::User(message) if message.text == "one"
    ));

    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session.session.session_id,
        })
        .await
        .expect("session view")
        .snapshot;
    assert_eq!(view.value.runs.len(), 2);
    assert!(view.value.queue.items.is_empty());
    assert!(view.observed_sequence >= 5);
    let around = runtime
        .get_conversation_page_around_run(GetConversationPageAroundRunRequest {
            session_id: view.value.session.session_id.clone(),
            run_id: view.value.runs[1].run_id.clone(),
            limit: 2,
        })
        .await
        .expect("page around run");
    assert!(around.snapshot.value.items.iter().any(|item| {
        matches!(item, ConversationItem::Assistant(message) if message.message_id == around.anchor_message_id)
    }));

    let ConversationItem::User(first_user) = &older.items[0] else {
        panic!("older page starts with user")
    };
    let rewritten = runtime
        .reenter_from_user_message(ReenterFromUserMessageRequest {
            session_id: view.value.session.session_id.clone(),
            message_id: first_user.message_id.clone(),
            message: "replacement".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("rewrite conversation");
    wait_for_terminal(
        &runtime,
        &view.value.session.session_id,
        &rewritten.run.run_id,
    )
    .await;
    let latest_after_rewrite = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: ConversationOwner::MainSession {
                session_id: view.value.session.session_id.clone(),
            },
            cursor: None,
            limit: 2,
        })
        .await
        .expect("latest page after rewrite")
        .snapshot
        .value;
    assert_eq!(latest_after_rewrite.generation, old_generation + 1);
    assert!(matches!(
        runtime
            .list_conversation_page(ListConversationPageRequest {
                owner: ConversationOwner::MainSession {
                    session_id: view.value.session.session_id,
                },
                cursor: Some(old_cursor),
                limit: 2,
            })
            .await,
        Err(RuntimeError::SnapshotStale)
    ));
}

#[tokio::test]
async fn queue_priority_and_interrupt_pause_resume_on_new_user_intent() {
    let entered = Arc::new(Notify::new());
    let runtime = runtime_with_tools(
        Arc::new(CancellationAwareModel {
            capabilities: model_capabilities(false),
            entered: entered.clone(),
        }),
        ToolSetSnapshot::default(),
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let active = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "active".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("active input");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("model entered");
    let second = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "second".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("second input");
    let third = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "third".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("third input");
    let before = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value
        .queue;
    assert_eq!(
        before
            .items
            .iter()
            .map(|item| item.input_id())
            .collect::<Vec<_>>(),
        vec![&second.input_id, &third.input_id]
    );

    let prioritized = runtime
        .prioritize_queued_input(PrioritizeQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: third.input_id.clone(),
            expected_revision: before.revision,
        })
        .await
        .expect("prioritize input")
        .queue;
    assert_eq!(prioritized.items[0].input_id(), &third.input_id);
    assert!(matches!(
        runtime
            .prioritize_queued_input(PrioritizeQueuedInputRequest {
                session_id: session_id.clone(),
                input_id: second.input_id,
                expected_revision: before.revision,
            })
            .await,
        Err(RuntimeError::QueueConflict)
    ));

    let interrupted = runtime
        .interrupt_run(InterruptRunRequest {
            session_id: session_id.clone(),
            run_id: active.run.run_id.clone(),
        })
        .await
        .expect("interrupt active run");
    assert_eq!(interrupted.queue.state, QueueExecutionState::PausedByUser);
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &active.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    let paused = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("paused session view")
        .snapshot
        .value
        .queue;
    assert_eq!(paused.state, QueueExecutionState::PausedByUser);
    assert_eq!(paused.items[0].input_id(), &third.input_id);
    runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "continue".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("new user intent");
    let resumed = runtime
        .get_session_view(GetSessionViewRequest { session_id })
        .await
        .expect("resumed session view")
        .snapshot
        .value
        .queue;
    assert_eq!(resumed.state, QueueExecutionState::Automatic);
}

#[tokio::test]
async fn tool_detail_is_loaded_by_stable_owner_message_and_call_ids() {
    let tool = ScriptedTool::succeed("detail_tool", json!({"saved": true}), OrderLog::new());
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("register tool");
    let tool_message_id = MessageId::new("assistant-detail-tool").expect("message id");
    let call_id = ToolCallId::new("detail-call").expect("call id");
    let runtime = runtime_with_tools(
        Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8_192,
            [
                ModelScript::Events(message_events(&AssistantMessage {
                    id: tool_message_id.clone(),
                    model: ModelIdentity::new(
                        ProviderId::new("fixture").expect("provider id"),
                        "fixture-model",
                    ),
                    parts: vec![AssistantPart::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: ToolName::new("detail_tool").expect("tool name"),
                        arguments: json!({"path": "report.txt"}),
                    })],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                })),
                ModelScript::Events(message_events(&assistant_text(
                    "assistant-detail-final",
                    "saved",
                ))),
            ],
        )),
        registry.snapshot(),
    );
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    set_auto_approval(&runtime, &session_id).await;
    let run = runtime
        .submit_input(SubmitInputRequest {
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "save a report".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .expect("submit");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &run.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );

    let page = runtime
        .get_conversation_page_around_run(GetConversationPageAroundRunRequest {
            session_id: session_id.clone(),
            run_id: run.run.run_id.clone(),
            limit: 8,
        })
        .await
        .expect("conversation page")
        .snapshot
        .value;
    let tool_event = page.items.iter().find_map(|item| {
        let ConversationItem::Assistant(message) = item else {
            return None;
        };
        message.segments.iter().find_map(|segment| {
            let assistant_protocol::AssistantSegment::ToolGroup { tools } = segment else {
                return None;
            };
            tools.first()
        })
    });
    assert!(matches!(
        tool_event.map(|event| &event.input),
        Some(assistant_protocol::ToolInputSnapshot::File { path, .. }) if path == "report.txt"
    ));

    let session_view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("session view")
        .snapshot
        .value;
    assert_eq!(session_view.file_references.len(), 1);
    assert_eq!(
        session_view.file_references[0].file.display_name,
        "report.txt"
    );
    assert_eq!(
        session_view.file_references[0].message_id.as_str(),
        tool_message_id.as_str()
    );
    assert_eq!(
        session_view.file_references[0].call_id.as_str(),
        call_id.as_str()
    );

    let detail = runtime
        .get_tool_detail(GetToolDetailRequest {
            owner: ConversationOwner::MainSession { session_id },
            message_id: assistant_protocol::MessageId::new(tool_message_id.as_str())
                .expect("protocol message id"),
            call_id: assistant_protocol::ToolCallId::new(call_id.as_str())
                .expect("protocol call id"),
        })
        .await
        .expect("tool detail")
        .snapshot
        .value;
    assert_eq!(detail.tool_name, "detail_tool");
    assert_eq!(
        detail.status,
        assistant_protocol::ToolActivityStatus::Completed
    );
    assert!(matches!(
        detail.input,
        assistant_protocol::ToolInputSnapshot::File { path, .. } if path == "report.txt"
    ));
    assert_eq!(detail.result_summary.as_deref(), Some("{\"saved\":true}"));
    assert_eq!(
        detail.request_json.as_deref(),
        Some("{\n  \"path\": \"report.txt\"\n}")
    );
    assert_eq!(
        detail.result_json.as_deref(),
        Some("{\n  \"saved\": true\n}")
    );
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].display_path.as_deref(), Some("report.txt"));
    let resolved = runtime
        .resolve_tool_file_resource(
            &detail.owner,
            &detail.message_id,
            &detail.files[0].resource_ref_id,
        )
        .await
        .expect("stable tool resource");
    assert!(resolved.path.ends_with("report.txt"));
    assert_eq!(resolved.display_name, "report.txt");
}
