//! 在实际 HTTP 编码边界对照三种本地摘要入口；只使用内存历史与 loopback 假模型。

use std::{collections::BTreeMap, time::Duration};

use agent_model::{
    GenerationConfig, ModelCallContext, ModelCapabilities, ProviderOptions, ReasoningConfig,
    ReasoningEffort, ToolChoiceCapabilities, collect_model_turn,
};
use agent_openai_compatible::{
    BearerCredential, ChatProtocolAdapter, OpenAiChatCompletionsService, OpenAiResponsesService,
    ResponsesProtocolAdapter, TransportTimeouts,
};
use agent_types::{
    AssistantMessage, AssistantMessagePurpose, AssistantPart, FinishReason, InternalContextPart,
    MessageId, ModelIdentity, PartId, ProviderId, TextPart, ToolCall, ToolCallId, ToolDefinition,
    ToolMessage, ToolName, ToolResult, ToolResultContent, ToolResultStatus, UserMessage, UserPart,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

use super::*;

#[tokio::test]
async fn local_summary_chat_http_requests_match_across_manual_parent_and_child() {
    assert_local_summary_http_requests(false).await;
}

#[tokio::test]
async fn local_summary_responses_http_requests_match_across_manual_parent_and_child() {
    assert_local_summary_http_requests(true).await;
}

async fn assert_local_summary_http_requests(responses: bool) {
    let (endpoint, server) = summary_server(responses).await;
    let provider = ProviderId::new("test").unwrap();
    let capabilities = ModelCapabilities {
        streaming: true,
        tool_calls: true,
        reasoning: true,
        tool_choice: ToolChoiceCapabilities::auto_only(),
        ..Default::default()
    };
    let timeouts = TransportTimeouts {
        connect: Duration::from_secs(5),
        request: Duration::from_secs(5),
    };
    let model: Arc<dyn ModelService> = if responses {
        Arc::new(
            OpenAiResponsesService::new_with_capabilities(
                &endpoint,
                BearerCredential::new("test-only"),
                "summary-test",
                128_000,
                ResponsesProtocolAdapter::openai_compatible(provider)
                    .with_tool_choice(ToolChoiceCapabilities::auto_only()),
                capabilities,
                timeouts,
            )
            .unwrap(),
        )
    } else {
        Arc::new(
            OpenAiChatCompletionsService::new_with_capabilities(
                &endpoint,
                BearerCredential::new("test-only"),
                "summary-test",
                128_000,
                ChatProtocolAdapter::openai_compatible(provider).with_reasoning_efforts(
                    Some("reasoning_effort"),
                    BTreeMap::from([(ReasoningEffort::High, json!("high"))]),
                ),
                capabilities,
                timeouts,
            )
            .unwrap(),
        )
    };
    assert!(!model.capabilities().tool_choice.none);
    let source = source_history();
    let original = source.clone();
    let system =
        SystemPromptSnapshot::new(vec!["frozen instruction".into(), "stable prefix".into()]);
    let mut provider_options = ProviderOptions::new();
    provider_options
        .insert("test", json!({"metadata": {"purpose": "f04-m0"}}))
        .unwrap();
    let config = ModelRequestConfig {
        generation: GenerationConfig {
            max_output_tokens: Some(99),
            ..Default::default()
        },
        reasoning: Some(ReasoningConfig {
            effort: Some(ReasoningEffort::High),
        }),
        provider_options,
        ..Default::default()
    };
    let normal = ModelRequest {
        system: system.clone(),
        conversation: source.clone(),
        tools: vec![ToolDefinition {
            name: ToolName::new("lookup").unwrap(),
            description: "look up facts".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }],
        tool_choice: ToolChoice::Auto,
        generation: config.generation.clone(),
        reasoning: config.reasoning.clone(),
        provider_options: config.provider_options.clone(),
    };
    let manual = RuntimeContextCompactor::for_manual(model.clone(), system.clone(), config);
    let ManualCompactionCandidate::Replacement { conversation, .. } = manual
        .compact_manual(source.clone(), source.clone(), CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("expected manual replacement")
    };
    assert_eq!(&conversation.messages[1..], &source.messages[2..]);
    for compactor in [
        RuntimeContextCompactor::for_parent(model.clone(), system.clone()),
        RuntimeContextCompactor::for_child(model.clone(), system.clone()),
    ] {
        let replacement = compactor
            .compact(
                source.clone(),
                source.clone(),
                normal.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(&replacement.messages[1..], &source.messages[2..]);
    }

    // 自动入口合法的 request-only 增补仍进入摘要请求，不反写源历史或候选。
    let mut augmented = normal.clone();
    augmented
        .conversation
        .messages
        .push(ConversationMessage::User(UserMessage {
            id: MessageId::new("request-only").unwrap(),
            origin: UserMessageOrigin::Runtime,
            transcript_visibility: TranscriptVisibility::Hidden,
            parts: vec![UserPart::InternalContext(InternalContextPart {
                id: PartId::new("request-only-part").unwrap(),
                boundary_id: "live-boundary".into(),
                kind: "live_context".into(),
                text: "request-only-marker".into(),
            })],
        }));
    let replacement = RuntimeContextCompactor::for_parent(model.clone(), system)
        .compact(
            source.clone(),
            source.clone(),
            augmented,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        !serde_json::to_string(&replacement)
            .unwrap()
            .contains("request-only-marker")
    );
    collect_model_turn(model.as_ref(), normal.clone(), ModelCallContext::default())
        .await
        .unwrap();
    assert_eq!(source, original);
    assert_eq!(normal.tools.len(), 1);

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 5);
    for request in &requests[..4] {
        assert!(request.get("tools").is_none());
        assert!(request.get("tool_choice").is_none());
        assert_eq!(
            request[if responses {
                "max_output_tokens"
            } else {
                "max_tokens"
            }],
            SUMMARY_OUTPUT_TOKENS
        );
        assert_eq!(request["metadata"], json!({"purpose": "f04-m0"}));
        assert_eq!(
            request[if responses {
                "reasoning"
            } else {
                "reasoning_effort"
            }],
            if responses {
                json!({"effort": "high"})
            } else {
                json!("high")
            }
        );
        let encoded = request.to_string();
        assert!(encoded.contains("frozen instruction"));
        assert!(encoded.contains("stable prefix"));
        assert!(encoded.contains("retained-tool-result"));
        assert!(encoded.contains("call-retained"));
    }
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[1], requests[2]);
    assert!(requests[3].to_string().contains("request-only-marker"));
    assert!(!requests[0].to_string().contains("request-only-marker"));
    assert_eq!(requests[4]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(
        requests[4][if responses {
            "max_output_tokens"
        } else {
            "max_tokens"
        }],
        99
    );
}

fn source_history() -> ConversationSnapshot {
    let user = |id: &str, text: String| {
        ConversationMessage::User(UserMessage {
            id: MessageId::new(id).unwrap(),
            origin: Default::default(),
            transcript_visibility: Default::default(),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(id).unwrap(),
                text,
            })],
        })
    };
    let assistant = |id: &str, parts, finish_reason| {
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new(id).unwrap(),
            purpose: AssistantMessagePurpose::ModelResponse,
            model: ModelIdentity {
                provider: ProviderId::new("test").unwrap(),
                model: "summary-test".into(),
            },
            parts,
            finish_reason,
            usage: None,
        })
    };
    ConversationSnapshot::new(vec![
        user("old-user", "old detail ".repeat(500)),
        assistant(
            "old-answer",
            vec![AssistantPart::Text(TextPart {
                id: PartId::new("old-answer-part").unwrap(),
                text: "old answer".into(),
            })],
            FinishReason::Stop,
        ),
        user("recent-user", "current task".into()),
        assistant(
            "retained-call",
            vec![AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("call-retained").unwrap(),
                name: ToolName::new("lookup").unwrap(),
                arguments: json!({}),
            })],
            FinishReason::ToolCalls,
        ),
        ConversationMessage::Tool(ToolMessage {
            id: MessageId::new("retained-result").unwrap(),
            result: ToolResult {
                call_id: ToolCallId::new("call-retained").unwrap(),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("retained-tool-result"),
                metadata: None,
            },
        }),
    ])
}

/// 每个请求完整收集 HTTP body 后才响应；超时仅约束失败测试，不用于猜测执行顺序。
async fn summary_server(responses: bool) -> (String, JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(15), async move {
            let mut requests = Vec::new();
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 4096];
                let header_end = loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0 && bytes.len() < 128 * 1024);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break end + 4; }
                };
                let header = std::str::from_utf8(&bytes[..header_end]).unwrap();
                assert!(header.starts_with(if responses { "POST /responses " } else { "POST /chat/completions " }));
                let length: usize = header.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                }).unwrap();
                assert!(length < 128 * 1024);
                while bytes.len() < header_end + length {
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                requests.push(serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap());
                let body = if responses {
                    include_str!("../../../agent-openai-compatible/fixtures/responses/openai/text.sse")
                } else {
                    concat!(
                        "data: {\"id\":\"summary-response\",\"model\":\"summary-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"short summary\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"id\":\"summary-response\",\"model\":\"summary-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                    )
                };
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(reply.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
            requests
        }).await.expect("local summary server completed")
    });
    (endpoint, task)
}
