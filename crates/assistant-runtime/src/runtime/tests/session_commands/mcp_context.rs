//! MCP 目录在输入、刷新、压缩与真实 Responses 编码之间的历史一致性回归。

use super::*;

#[tokio::test]
async fn mcp_directory_is_persisted_once_and_restored_by_compaction() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text(
                "mcp-first",
                &"history ".repeat(600),
            ))),
            ModelScript::Events(message_events(&assistant_text("mcp-second", "second"))),
            ModelScript::Events(message_events(&assistant_text("mcp-summary", "summary"))),
            ModelScript::Events(message_events(&assistant_text("mcp-third", "third"))),
            ModelScript::Events(message_events(&assistant_text(
                "mcp-second-summary",
                "summary",
            ))),
        ],
    ));
    let runtime = with_mcp(runtime(model.clone()));
    runtime.bootstrap_mcp().await.unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    for text in ["first", "second"] {
        let input = runtime
            .submit_started_input(input_request(&session_id, text))
            .await
            .unwrap();
        assert_eq!(
            wait_for_terminal(&runtime, &session_id, &input.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
    }
    let requests = model.take_requests();
    let stored = runtime.store.load_conversation(&session_id).await.unwrap();
    assert_eq!(requests[0].conversation.messages, stored.messages[..1]);
    assert_eq!(requests[1].conversation.messages, stored.messages[..3]);
    assert_eq!(
        serde_json::to_string(&stored)
            .unwrap()
            .matches("MCP_SERVER_DIRECTORY_V1")
            .count(),
        1
    );
    runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: IdempotencyKey::new("mcp-compact").unwrap(),
            expected_generation: 1,
        })
        .await
        .unwrap();
    let compacted = runtime.conversation_snapshot(&session_id).await.unwrap();
    assert!(matches!(
        compacted.messages[0],
        ConversationMessage::ContextSummary(_)
    ));
    assert_eq!(
        serde_json::to_string(&compacted)
            .unwrap()
            .matches("MCP_SERVER_DIRECTORY_V1")
            .count(),
        1
    );
    let input = runtime
        .submit_started_input(input_request(&session_id, "third"))
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &input.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2); // 摘要调用和压缩后的普通调用。
    assert_eq!(
        serde_json::to_string(&requests[1].conversation)
            .unwrap()
            .matches("MCP_SERVER_DIRECTORY_V1")
            .count(),
        1
    );
    let current = runtime.conversation_snapshot(&session_id).await.unwrap();
    assert_eq!(
        requests[1].conversation.messages,
        current.messages[..current.messages.len() - 1]
    );
    runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: session_id.clone(),
            operation_id: IdempotencyKey::new("mcp-compact-again").unwrap(),
            expected_generation: 2,
        })
        .await
        .unwrap();
    let second = runtime.conversation_snapshot(&session_id).await.unwrap();
    assert_eq!(
        serde_json::to_string(&second)
            .unwrap()
            .matches("MCP_SERVER_DIRECTORY_V1")
            .count(),
        1
    );
    let history = runtime.store.load_conversation(&session_id).await.unwrap();
    let restored_ids = history
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::User(user)
                if user.transcript_visibility == agent_types::TranscriptVisibility::Hidden =>
            {
                Some(user.id.clone())
            }
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        restored_ids.len(),
        2,
        "each compaction restoration has its own persistent identity"
    );
}

// 真实 Runtime 装配与 Responses Adapter 共用一条测试链；只在 HTTP Transport 边界替换网络。
// 不能用手工拼装的 Conversation 代替本测试，否则会漏掉请求临时注入与持久历史的差异。
#[derive(Default)]
struct McpResponsesTransport {
    requests: Mutex<Vec<serde_json::Value>>,
}

impl agent_openai_compatible::Transport for McpResponsesTransport {
    fn execute<'a>(
        &'a self,
        request: agent_openai_compatible::TransportRequest,
    ) -> agent_openai_compatible::TransportFuture<'a> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(serde_json::from_slice(&request.body).unwrap());
        let index = requests.len();
        let fixture = include_str!(
            "../../../../../agent-openai-compatible/fixtures/responses/openai/text.sse"
        )
        .replace("resp_text_1", &format!("resp_mcp_{index}"))
        .replace("msg_text_1", &format!("msg_mcp_{index}"));
        Box::pin(async move {
            Ok(agent_openai_compatible::TransportResponse {
                status: 200,
                headers: vec![],
                body: Box::pin(futures_util::stream::once(async move {
                    Ok(fixture.into_bytes())
                })),
            })
        })
    }
}

#[tokio::test]
async fn mcp_runtime_uses_previous_response_id_and_sends_only_the_next_input() {
    use agent_openai_compatible::{
        BearerCredential, OpenAiResponsesService, ResponsesProtocolAdapter,
    };
    let transport = Arc::new(McpResponsesTransport::default());
    let service = OpenAiResponsesService::with_transport(
        "https://offline.test/v1",
        BearerCredential::new("unused-test-credential"),
        "fixture-model",
        8_192,
        ResponsesProtocolAdapter::openai(),
        transport.clone(),
    )
    .unwrap()
    .with_server_history("mcp-runtime-test")
    .unwrap();
    let runtime = with_mcp(runtime(Arc::new(service)));
    runtime.bootstrap_mcp().await.unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    for text in ["first", "next", "third"] {
        let accepted = runtime
            .submit_started_input(input_request(&session_id, text))
            .await
            .unwrap();
        assert_eq!(
            wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed
        );
    }
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["store"], true);
    assert!(requests[0].get("previous_response_id").is_none());
    assert!(requests[0].to_string().contains("MCP_SERVER_DIRECTORY_V1"));
    for (index, request) in requests.iter().enumerate().skip(1) {
        assert_eq!(request["previous_response_id"], format!("resp_mcp_{index}"));
        assert_eq!(request["input"].as_array().unwrap().len(), 1);
        assert!(!request.to_string().contains("MCP_SERVER_DIRECTORY_V1"));
    }
}

#[tokio::test]
async fn mcp_directory_survives_automatic_compaction_and_overflow_without_new_run() {
    for overflow in [false, true] {
        let mut first = assistant_text("mcp-auto-first", &"history ".repeat(500));
        if !overflow {
            first.usage = Some(agent_types::TokenUsage {
                input_tokens: 7000,
                output_tokens: 500,
                total_tokens: 7500,
                cached_input_tokens: None,
                reasoning_tokens: None,
            });
        }
        let mut scripts = vec![ModelScript::Events(message_events(&first))];
        if overflow {
            scripts.push(ModelScript::FailEstablishment(
                ModelError::ContextOverflow {
                    message: "fixture overflow".into(),
                },
            ));
        }
        scripts.extend(
            ["mcp-auto-summary", "mcp-auto-second", "mcp-auto-third"]
                .map(|id| ModelScript::Events(message_events(&assistant_text(id, "done")))),
        );
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            scripts,
        ));
        let runtime = with_mcp(runtime(model.clone()));
        runtime.bootstrap_mcp().await.unwrap();
        let session_id = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        for text in ["first", "second", "third"] {
            let accepted = runtime
                .submit_started_input(input_request(&session_id, text))
                .await
                .unwrap();
            assert_eq!(
                wait_for_terminal(&runtime, &session_id, &accepted.run.run_id)
                    .await
                    .status,
                assistant_protocol::RunStatus::Completed
            );
        }
        let requests = model.take_requests();
        let last = &requests.last().unwrap().conversation;
        assert!(matches!(
            last.messages[0],
            ConversationMessage::ContextSummary(_)
        ));
        assert_eq!(
            serde_json::to_string(last)
                .unwrap()
                .matches("MCP_SERVER_DIRECTORY_V1")
                .count(),
            1
        );
        let stored = runtime.store.load_conversation(&session_id).await.unwrap();
        let projected = crate::execution_context_from_product_history(&stored);
        assert_eq!(
            last.messages,
            projected.messages[..projected.messages.len() - 1]
        );
        assert_eq!(
            runtime
                .session_for_test(&session_id)
                .await
                .run_snapshots()
                .unwrap()
                .len(),
            3
        );
    }
}

struct EmptyableMcpSource(AtomicBool);
impl crate::McpConfigSource for EmptyableMcpSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        if self.0.load(Ordering::SeqCst) {
            Box::pin(std::future::ready(ConfigSourceLoad::Document(
                ConfigDocument::new(r#"{"mcpServers":{}}"#.into(), "empty-mcp".into()),
            )))
        } else {
            StaticMcpTestSource.load()
        }
    }
    fn replace(&self, revision: Option<String>, document: String) -> ConfigSourceReplaceFuture<'_> {
        StaticMcpTestSource.replace(revision, document)
    }
}

#[tokio::test]
async fn mcp_refresh_persists_empty_directory_after_the_last_server_is_removed() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        ["mcp-before-removal", "mcp-after-removal"]
            .map(|id| ModelScript::Events(message_events(&assistant_text(id, "done")))),
    ));
    let source = Arc::new(EmptyableMcpSource(AtomicBool::new(false)));
    let runtime = runtime(model.clone()).with_mcp_services(
        source.clone(),
        Arc::new(WorkingMcpTestFactory {
            connection: Arc::new(WorkingMcpTestConnection {
                calls: AtomicUsize::new(0),
            }),
        }),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let accepted = runtime
        .submit_started_input(input_request(&session_id, "before"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &accepted.run.run_id).await;
    source.0.store(true, Ordering::SeqCst);
    let accepted = runtime
        .submit_session_command(refresh_request(&session_id, "remove-server"))
        .await
        .unwrap()
        .accepted;
    wait_for_command(&runtime, &session_id, &accepted.input_id).await;
    assert_eq!(
        model.take_requests().len(),
        1,
        "refresh never calls the model"
    );
    let history = runtime.store.load_conversation(&session_id).await.unwrap();
    let ConversationMessage::User(refresh) = history.messages.last().unwrap() else {
        panic!("refresh user");
    };
    assert_eq!(history.messages.len(), 3);
    let directory = refresh
        .parts
        .iter()
        .find_map(|part| match part {
            UserPart::InternalContext(part) if part.kind == "mcp_server_directory" => {
                Some(&part.text)
            }
            _ => None,
        })
        .unwrap();
    assert!(directory.ends_with("[]"));
    assert!(!directory.contains("github"));
    let accepted = runtime
        .submit_started_input(input_request(&session_id, "after"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &accepted.run.run_id).await;
    let requests = model.take_requests();
    assert_eq!(requests[0].conversation.messages[..3], history.messages);
    assert_eq!(requests[0].conversation.messages.len(), 4);
}

#[tokio::test]
async fn mcp_reentered_input_gets_persistent_context_before_the_model_call() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        ["before-reentry", "after-reentry"]
            .map(|id| ModelScript::Events(message_events(&assistant_text(id, "done")))),
    ));
    let runtime = with_mcp(runtime(model.clone()));
    runtime.bootstrap_mcp().await.unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let accepted = runtime
        .submit_started_input(input_request(&session_id, "original"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &accepted.run.run_id).await;
    let history = runtime.store.load_conversation(&session_id).await.unwrap();
    let ConversationMessage::User(user) = &history.messages[0] else {
        panic!("first user");
    };
    let accepted = runtime
        .reenter_from_user_message({
            let (expected_generation, expected_message_count) =
                edit_basis(&runtime, &session_id.clone()).await;
            assistant_protocol::ReenterFromUserMessageRequest {
                session_id: session_id.clone(),
                message_id: assistant_protocol::MessageId::new(user.id.as_str()).unwrap(),
                message: "replacement".into(),
                attachment_ids: vec![],
                variant: AgentVariant::Build,
                idempotency_key: assistant_protocol::IdempotencyKey::new(
                    crate::id::generate("edit-test").unwrap(),
                )
                .unwrap(),

                expected_generation,
                expected_message_count,
                quotes: Vec::new(),
                skill_edit: assistant_protocol::UserMessageSkillEdit::Retain,
                mcp_edit: assistant_protocol::UserMessageMcpEdit::Retain,
            }
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
    assert_eq!(requests.len(), 2);
    let history = runtime.store.load_conversation(&session_id).await.unwrap();
    assert_eq!(
        requests[1].conversation.messages,
        history.messages[..history.messages.len() - 1]
    );
    assert_eq!(
        serde_json::to_string(&requests[1].conversation)
            .unwrap()
            .matches("MCP_SERVER_DIRECTORY_V1")
            .count(),
        1
    );
}

#[tokio::test]
async fn controller_session_dynamically_discloses_device_mcp_and_preserves_cache_when_unchanged() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        ["turn-1", "turn-2", "turn-3", "turn-4", "turn-5"]
            .map(|id| ModelScript::Events(message_events(&assistant_text(id, "ok")))),
    ));
    let factory = Arc::new(RecordingMcpFactory::default());
    let runtime = runtime(model.clone()).with_mcp_services(
        Arc::new(MutableJsonMcpSource::new(r#"{"mcpServers":{}}"#)),
        factory.clone(),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.unwrap();
    let controller = runtime
        .ensure_controller_session(false)
        .await
        .unwrap()
        .summary()
        .unwrap()
        .session_id;

    // Turn 1: zero MCP servers configured -> no directory marker written.
    let run_1 = runtime
        .submit_started_input(input_request(&controller, "t1"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &controller, &run_1.run.run_id).await;
    let requests = model.take_requests();
    assert!(
        !serde_json::to_string(&requests[0].conversation)
            .unwrap()
            .contains("MCP_SERVER_DIRECTORY_V1")
    );

    // Connect a paired device declaring its LAN HTTP MCP endpoint.
    let device_id = assistant_protocol::DeviceId::new("device-DynamicMcp01").unwrap();
    let device_key = crate::device_mcp_server_key(&device_id, Some("esp-pika")).unwrap();
    runtime
        .sync_device_mcp_servers(
            &device_id,
            vec![device_mcp_definition(
                &device_id,
                &device_key,
                "http://192.168.10.20:8080/mcp",
                "digest-ip-1",
            )],
        )
        .await
        .unwrap();

    // Turn 2 in existing controller session immediately sees the newly added device MCP.
    let run_2 = runtime
        .submit_started_input(input_request(&controller, "t2"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &controller, &run_2.run.run_id).await;
    // Turn 3 with unchanged MCP directory must NOT append another directory boundary.
    let run_3 = runtime
        .submit_started_input(input_request(&controller, "t3"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &controller, &run_3.run.run_id).await;

    let requests = model.take_requests();
    let req2_json = serde_json::to_string(&requests[0].conversation).unwrap();
    assert_eq!(req2_json.matches("MCP_SERVER_DIRECTORY_V1").count(), 1);
    assert!(req2_json.contains(device_key.as_str()));
    let req3_json = serde_json::to_string(&requests[1].conversation).unwrap();
    assert_eq!(req3_json.matches("MCP_SERVER_DIRECTORY_V1").count(), 1);

    // Device disconnects -> Turn 4 appends empty directory once; Turn 5 preserves it without repeating.
    runtime.mark_device_mcp_offline(&device_id).await.unwrap();
    let run_4 = runtime
        .submit_started_input(input_request(&controller, "t4"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &controller, &run_4.run.run_id).await;
    let run_5 = runtime
        .submit_started_input(input_request(&controller, "t5"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &controller, &run_5.run.run_id).await;

    let requests = model.take_requests();
    let req4_json = serde_json::to_string(&requests[0].conversation).unwrap();
    assert_eq!(req4_json.matches("MCP_SERVER_DIRECTORY_V1").count(), 2);
    assert!(req4_json.contains("No MCP servers are currently available"));
    let req5_json = serde_json::to_string(&requests[1].conversation).unwrap();
    assert_eq!(req5_json.matches("MCP_SERVER_DIRECTORY_V1").count(), 2);
}

#[tokio::test]
async fn device_mcp_refresh_is_single_target_and_retires_stale_ip_on_failure() {
    let factory = Arc::new(RecordingMcpFactory::default());
    let runtime = runtime(empty_model()).with_mcp_services(
        Arc::new(StaticMcpTestSource),
        factory.clone(),
        Arc::new(crate::mcp::UnavailableMcpImageMaterializer),
    );
    runtime.bootstrap_mcp().await.unwrap();
    assert_eq!(factory.connect_count("github"), 1);

    let device_id = assistant_protocol::DeviceId::new("device-TargetOnly01").unwrap();
    let device_key = crate::device_mcp_server_key(&device_id, Some("esp-pika")).unwrap();
    runtime
        .sync_device_mcp_servers(
            &device_id,
            vec![device_mcp_definition(
                &device_id,
                &device_key,
                "http://192.168.10.21:8080/mcp",
                "digest-ip-1",
            )],
        )
        .await
        .unwrap();
    assert_eq!(factory.connect_count("github"), 1);
    assert_eq!(factory.connect_count(device_key.as_str()), 1);

    // Next reconnect changes IP (digest) and fails connect: old IP connection must be retired, not retained.
    *factory.fail_url.lock().unwrap() = Some("http://192.168.10.22:8080/mcp".to_owned());
    runtime
        .sync_device_mcp_servers(
            &device_id,
            vec![device_mcp_definition(
                &device_id,
                &device_key,
                "http://192.168.10.22:8080/mcp",
                "digest-ip-2",
            )],
        )
        .await
        .unwrap();
    assert_eq!(factory.connect_count("github"), 1);
    assert_eq!(factory.connect_count(device_key.as_str()), 2);
    let options = runtime
        .list_mcp_server_options(assistant_protocol::ListMcpServerOptionsRequest {
            variant: AgentVariant::Build,
            context: assistant_protocol::McpServerOptionsContext::NewSession { workspace_id: None },
        })
        .await
        .unwrap()
        .servers;
    assert!(options.iter().any(|s| s.server_key.as_str() == "github"));
    assert!(
        !options.iter().any(|s| s.server_key == device_key),
        "failed IP change must retire old IP connection instead of keeping stale endpoint active"
    );

    runtime.remove_device_mcp_servers(&device_id).await.unwrap();
    assert_eq!(factory.connect_count("github"), 1);
}

#[derive(Default)]
struct RecordingMcpFactory {
    counts: Mutex<BTreeMap<String, usize>>,
    fail_url: Mutex<Option<String>>,
}

impl RecordingMcpFactory {
    fn connect_count(&self, key: &str) -> usize {
        self.counts
            .lock()
            .unwrap()
            .get(key)
            .copied()
            .unwrap_or_default()
    }
}

impl crate::McpConnectionFactory for RecordingMcpFactory {
    fn connect(
        &self,
        server: crate::McpServerConfig,
        _options: crate::McpConnectionOptions,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> crate::McpConnectionFuture<'_, Arc<dyn crate::McpConnection>> {
        *self
            .counts
            .lock()
            .unwrap()
            .entry(server.server_key().to_string())
            .or_default() += 1;
        let blocked = matches!(
            server.transport(),
            crate::McpServerTransportConfig::StreamableHttp { url, .. }
                if self.fail_url.lock().unwrap().as_deref() == Some(url)
        );
        if blocked {
            return Box::pin(std::future::ready(Err(crate::McpConnectionError::new(
                crate::McpConnectionFailureKind::Connect,
                "injected connect failure",
            ))));
        }
        Box::pin(std::future::ready(Ok(Arc::new(WorkingMcpTestConnection {
            calls: AtomicUsize::new(0),
        })
            as Arc<dyn crate::McpConnection>)))
    }
}

struct MutableJsonMcpSource {
    content: Mutex<String>,
}

impl MutableJsonMcpSource {
    fn new(content: &str) -> Self {
        Self {
            content: Mutex::new(content.to_owned()),
        }
    }
}

impl crate::McpConfigSource for MutableJsonMcpSource {
    fn load(&self) -> crate::ConfigSourceFuture<'_> {
        let text = self.content.lock().unwrap().clone();
        Box::pin(std::future::ready(crate::ConfigSourceLoad::Document(
            crate::ConfigDocument::new(text, "rev-1".to_owned()),
        )))
    }

    fn replace(
        &self,
        _expected_revision: Option<String>,
        document: String,
    ) -> crate::ConfigSourceReplaceFuture<'_> {
        *self.content.lock().unwrap() = document.clone();
        Box::pin(std::future::ready(crate::ConfigSourceReplace::Applied(
            crate::ConfigDocument::new(document, "rev-2".to_owned()),
        )))
    }
}

fn device_mcp_definition(
    device_id: &assistant_protocol::DeviceId,
    server_key: &McpServerKey,
    url: &str,
    digest: &str,
) -> crate::ManagedMcpServerDefinition {
    crate::ManagedMcpServerDefinition {
        source: assistant_protocol::McpServerSource::Device(device_id.to_string()),
        server_key: server_key.clone(),
        definition_digest: digest.to_owned(),
        source_enabled: true,
        source_available: true,
        ready: true,
        raw: json!({
            "displayName": "ESP-Pika Gateway",
            "description": "Device MCP",
            "url": url,
            "enabled": true
        })
        .as_object()
        .unwrap()
        .clone(),
        parameter_bindings: BTreeMap::new(),
        parameters: Vec::new(),
        plugin_settings_revision: None,
        plugin_sha256: None,
        configuration: None,
    }
}
