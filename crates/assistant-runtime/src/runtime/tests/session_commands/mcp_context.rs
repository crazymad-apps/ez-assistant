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
        .reenter_from_user_message(assistant_protocol::ReenterFromUserMessageRequest {
            session_id: session_id.clone(),
            message_id: assistant_protocol::MessageId::new(user.id.as_str()).unwrap(),
            message: "replacement".into(),
            attachment_ids: vec![],
            variant: AgentVariant::Build,
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
