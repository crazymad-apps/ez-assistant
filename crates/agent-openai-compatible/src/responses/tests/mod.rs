use std::sync::Arc;

use agent_model::{
    GenerationConfig, ModelCallContext, ModelError, ModelEvent, ModelRequest, ModelService,
    ProviderOptions, SystemPromptSnapshot,
};
use agent_testkit::{BodyStep, EventCollector, RecordedResponse, RecordedTransport};
use agent_types::{
    AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot, FileReference,
    FileReferencesPart, FinishReason, InternalContextPart, MessageId, ModelIdentity,
    OpaqueProviderState, PartId, ProviderId, ReasoningPart, TextPart, TokenUsage, ToolCall,
    ToolCallId, ToolChoice, ToolDefinition, ToolImageReference, ToolMessage, ToolName, ToolResult,
    ToolResultContent, ToolResultPart, ToolResultStatus, TranscriptVisibility, UserMessage,
    UserMessageOrigin, UserPart,
};
use serde_json::{Value, json};

use crate::{BearerCredential, OpenAiResponsesService};

use super::{
    ResponsesProtocolAdapter, decode::decode_response, encode::encode_request_with_images,
    stream::ResponsesAssembler,
};

const TEXT_SSE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/responses/openai/text.sse"
));
const FUNCTION_SSE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/responses/openai/function_calls.sse"
));

fn provider(value: &str) -> ProviderId {
    ProviderId::new(value).expect("provider")
}

fn message_id(value: &str) -> MessageId {
    MessageId::new(value).expect("message id")
}

fn part_id(value: &str) -> PartId {
    PartId::new(value).expect("part id")
}

fn call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("call id")
}

fn tool_name(value: &str) -> ToolName {
    ToolName::new(value).expect("tool name")
}

fn request(conversation: Vec<ConversationMessage>) -> ModelRequest {
    ModelRequest {
        system: SystemPromptSnapshot::default(),
        conversation: ConversationSnapshot::new(conversation),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: ProviderOptions::new(),
    }
}

fn user(id: &str, text: &str) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        origin: Default::default(),
        transcript_visibility: Default::default(),
        id: message_id(id),
        parts: vec![UserPart::Text(TextPart {
            id: part_id(&format!("{id}-text")),
            text: text.to_owned(),
        })],
    })
}

fn runtime_user(id: &str, text: &str) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        origin: UserMessageOrigin::Runtime,
        transcript_visibility: TranscriptVisibility::Hidden,
        id: message_id(id),
        parts: vec![UserPart::InternalContext(
            InternalContextPart::new(
                part_id(&format!("{id}-internal")),
                format!("boundary-{id}"),
                "runtime_context",
                text,
            )
            .expect("internal context"),
        )],
    })
}

fn tool_image_request() -> (
    ModelRequest,
    ToolImageReference,
    agent_model::PreparedModelImages,
) {
    let image = ToolImageReference::new(format!("{}.png", "a".repeat(64)), "image/png")
        .expect("image reference");
    let request = request(vec![
        user("user_1", "inspect"),
        ConversationMessage::Assistant(AssistantMessage {
            id: message_id("assistant_image"),
            model: ModelIdentity::new(provider("openai"), "gpt-test"),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: call_id("call_image"),
                name: tool_name("read_image"),
                arguments: json!({"path":"chart.png"}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_image"),
            result: ToolResult {
                call_id: call_id("call_image"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![
                    ToolResultPart::text("chart"),
                    ToolResultPart::image(image.clone()),
                ])
                .expect("result"),
                metadata: None,
            },
        }),
    ]);
    let mut images = agent_model::PreparedModelImages::default();
    images.insert_tool_image(
        image.relative_path().to_owned(),
        agent_model::PreparedModelImage {
            media_type: "image/png".to_owned(),
            bytes: Arc::from([1_u8, 2, 3]),
        },
    );
    (request, image, images)
}

fn adapter() -> ResponsesProtocolAdapter {
    ResponsesProtocolAdapter::openai_compatible(provider("fixture"))
}

fn feed_sse(document: &str) -> Result<Vec<ModelEvent>, ModelError> {
    feed_sse_with_adapter(document, adapter(), "fixture-model")
}

fn feed_sse_with_adapter(
    document: &str,
    adapter: ResponsesProtocolAdapter,
    model: &str,
) -> Result<Vec<ModelEvent>, ModelError> {
    let mut assembler = ResponsesAssembler::new(adapter, model.to_owned());
    let mut events = Vec::new();
    for block in document.split("\n\n") {
        let Some(data) = block.strip_prefix("data: ") else {
            continue;
        };
        if data.trim() == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(data).expect("fixture event JSON");
        events.extend(assembler.push(&value)?);
    }
    assembler.finalize()?;
    Ok(events)
}

fn bound_adapter(
    adapter: ResponsesProtocolAdapter,
    base_url: &str,
    model: &str,
) -> ResponsesProtocolAdapter {
    let fingerprint = crate::shared::route_fingerprint(
        adapter.provider.as_str(),
        adapter.protocol.as_str(),
        base_url,
        model,
    );
    adapter.bind_route(fingerprint)
}

#[test]
fn request_is_stateless_streaming_and_rebuilds_complete_local_history() {
    let mut request = request(vec![
        user("user_1", "look up Tokyo"),
        ConversationMessage::Assistant(AssistantMessage {
            id: message_id("assistant_1"),
            model: ModelIdentity::new(provider("fixture"), "fixture-model"),
            parts: vec![
                AssistantPart::Reasoning(ReasoningPart {
                    id: part_id("reasoning_1"),
                    text: "Need a lookup".to_owned(),
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: call_id("call_1"),
                    name: tool_name("lookup"),
                    arguments: json!({"city": "Tokyo"}),
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_1"),
            result: ToolResult {
                call_id: call_id("call_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![
                    ToolResultPart::text("sunny"),
                    ToolResultPart::json(json!({"temperature": 26})),
                ])
                .expect("result"),
                metadata: None,
            },
        }),
        runtime_user("user_2", "summarize"),
    ]);
    request.system = SystemPromptSnapshot::new(vec!["one".to_owned(), "two".to_owned()]);
    let encoded = encode_request_with_images(
        &request,
        &agent_model::PreparedModelImages::default(),
        &adapter(),
        "fixture-model",
    )
    .expect("encode");
    let value = serde_json::to_value(encoded).expect("json");

    assert_eq!(value["store"], false);
    assert_eq!(value["stream"], true);
    assert_eq!(value["instructions"], "one\n\ntwo");
    assert!(value.get("previous_response_id").is_none());
    assert!(value.get("conversation").is_none());
    assert_eq!(value["input"].as_array().expect("input").len(), 5);
    assert_eq!(value["input"][1]["type"], "reasoning");
    assert_eq!(value["input"][2]["type"], "function_call");
    assert_eq!(value["input"][2]["call_id"], "call_1");
    assert_eq!(value["input"][3]["type"], "function_call_output");
    assert_eq!(value["input"][3]["output"], "sunny\n{\"temperature\":26}");
    assert_eq!(value["input"][4]["role"], "user");
    assert_eq!(value["input"][4]["content"][0]["text"], "summarize");
    assert!(value["input"][4].get("origin").is_none());
    assert!(value["input"][4].get("transcript_visibility").is_none());
}

#[test]
fn user_images_keep_their_original_message_position() {
    let path = "/session/attachments/red.png";
    let request = request(vec![ConversationMessage::User(UserMessage {
        origin: Default::default(),
        transcript_visibility: Default::default(),
        id: message_id("user_image"),
        parts: vec![UserPart::FileReferences(FileReferencesPart {
            id: part_id("files_1"),
            files: vec![FileReference {
                original_name: "red.png".to_owned(),
                readable_path: path.to_owned(),
            }],
        })],
    })]);
    let mut images = agent_model::PreparedModelImages::default();
    images.insert_file_reference(
        path.to_owned(),
        agent_model::PreparedModelImage {
            media_type: "image/png".to_owned(),
            bytes: Arc::from([1_u8, 2, 3]),
        },
    );
    let value = serde_json::to_value(
        encode_request_with_images(&request, &images, &adapter(), "fixture-model")
            .expect("encode image"),
    )
    .expect("json");
    assert_eq!(value["input"][0]["role"], "user");
    assert_eq!(value["input"][0]["content"][0]["type"], "input_image");
    assert_eq!(
        value["input"][0]["content"][0]["image_url"],
        "data:image/png;base64,AQID"
    );
}

#[test]
fn official_function_output_parts_carry_native_tool_images() {
    let (request, _image, images) = tool_image_request();
    let adapter = ResponsesProtocolAdapter::openai()
        .with_tool_image_projection(agent_model::ToolImageProjection::NativeFunctionOutput);
    let value = serde_json::to_value(
        encode_request_with_images(&request, &images, &adapter, "gpt-test").expect("native output"),
    )
    .expect("json");
    assert_eq!(value["input"].as_array().expect("input").len(), 3);
    assert_eq!(value["input"][2]["type"], "function_call_output");
    assert_eq!(value["input"][2]["output"][0]["type"], "input_text");
    assert_eq!(value["input"][2]["output"][1]["type"], "input_image");
}

#[test]
fn qwen_aggregates_tool_images_after_all_function_outputs() {
    let (request, _image, images) = tool_image_request();
    let value = serde_json::to_value(
        encode_request_with_images(
            &request,
            &images,
            &ResponsesProtocolAdapter::qwen(),
            "qwen3.8-max",
        )
        .expect("Qwen aggregated output"),
    )
    .expect("json");
    let input = value["input"].as_array().expect("input");
    assert_eq!(input.len(), 4);
    assert_eq!(input[2]["type"], "function_call_output");
    assert!(input[2]["output"].is_string());
    assert_eq!(input[3]["role"], "user");
    assert_eq!(input[3]["content"][0]["type"], "input_text");
    assert_eq!(input[3]["content"][1]["type"], "input_image");
}

#[test]
fn qwen_batches_all_function_outputs_before_one_ordered_image_envelope() {
    let first = ToolImageReference::new(format!("{}.png", "a".repeat(64)), "image/png")
        .expect("first image");
    let second = ToolImageReference::new(format!("{}.png", "b".repeat(64)), "image/png")
        .expect("second image");
    let request = request(vec![
        user("user_batch", "inspect both"),
        ConversationMessage::Assistant(AssistantMessage {
            id: message_id("assistant_batch"),
            model: ModelIdentity::new(provider("dashscope"), "qwen3.8-max"),
            parts: vec![
                AssistantPart::ToolCall(ToolCall {
                    id: call_id("call_a"),
                    name: tool_name("read_image"),
                    arguments: json!({"path":"a.png"}),
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: call_id("call_b"),
                    name: tool_name("read_image"),
                    arguments: json!({"path":"b.png"}),
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_a"),
            result: ToolResult {
                call_id: call_id("call_a"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![ToolResultPart::image(first.clone())])
                    .expect("first result"),
                metadata: None,
            },
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_b"),
            result: ToolResult {
                call_id: call_id("call_b"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![ToolResultPart::image(second.clone())])
                    .expect("second result"),
                metadata: None,
            },
        }),
    ]);
    let mut images = agent_model::PreparedModelImages::default();
    for image in [&first, &second] {
        images.insert_tool_image(
            image.relative_path().to_owned(),
            agent_model::PreparedModelImage {
                media_type: "image/png".to_owned(),
                bytes: Arc::from([1_u8, 2, 3]),
            },
        );
    }
    let value = serde_json::to_value(
        encode_request_with_images(
            &request,
            &images,
            &ResponsesProtocolAdapter::qwen(),
            "qwen3.8-max",
        )
        .expect("Qwen batch output"),
    )
    .expect("json");
    let input = value["input"].as_array().expect("input");
    assert_eq!(input.len(), 6);
    assert_eq!(input[1]["call_id"], "call_a");
    assert_eq!(input[2]["call_id"], "call_b");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_a");
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["call_id"], "call_b");
    assert_eq!(input[5]["role"], "user");
    assert_eq!(input[5]["content"].as_array().expect("envelope").len(), 4);
    assert_eq!(input[5]["content"][0]["type"], "input_text");
    assert_eq!(input[5]["content"][1]["type"], "input_image");
    assert_eq!(input[5]["content"][2]["type"], "input_text");
    assert_eq!(input[5]["content"][3]["type"], "input_image");
}

#[test]
fn kimi_keeps_tool_images_inside_native_function_output() {
    let (request, _image, images) = tool_image_request();
    let value = serde_json::to_value(
        encode_request_with_images(&request, &images, &ResponsesProtocolAdapter::kimi(), "k3")
            .expect("Kimi native output"),
    )
    .expect("json");
    let input = value["input"].as_array().expect("input");
    assert_eq!(input.len(), 3);
    assert_eq!(input[2]["type"], "function_call_output");
    assert_eq!(input[2]["output"][0]["type"], "input_text");
    assert_eq!(input[2]["output"][1]["type"], "input_image");
}

#[test]
fn tool_choice_and_function_schema_are_checked_before_transport() {
    let mut request = request(vec![user("user_tools", "use lookup")]);
    request.tools = vec![ToolDefinition {
        name: tool_name("lookup"),
        description: "lookup a city".to_owned(),
        input_schema: json!({
            "type":"object",
            "properties":{"city":{"type":"string"}},
            "required":["city"]
        }),
    }];
    request.tool_choice = ToolChoice::Required;
    let value = serde_json::to_value(
        encode_request_with_images(
            &request,
            &agent_model::PreparedModelImages::default(),
            &ResponsesProtocolAdapter::openai(),
            "gpt-test",
        )
        .expect("tool request"),
    )
    .expect("json");
    assert_eq!(value["tool_choice"], "required");
    assert_eq!(value["tools"][0]["type"], "function");

    let error = encode_request_with_images(
        &request,
        &agent_model::PreparedModelImages::default(),
        &adapter(),
        "fixture-model",
    )
    .expect_err("conservative route rejects required");
    assert!(matches!(error, ModelError::Config(_)));
}

#[test]
fn official_style_text_fixture_maps_usage_and_one_terminal() {
    let events = feed_sse(TEXT_SSE).expect("text fixture");
    assert!(matches!(
        events.first(),
        Some(ModelEvent::TurnStarted { .. })
    ));
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    let ModelEvent::TurnFinished { message } = events.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert_eq!(message.finish_reason, FinishReason::Stop);
    assert_eq!(
        message.parts,
        vec![AssistantPart::Text(TextPart {
            id: part_id("msg_text_1:content:0"),
            text: "Hello from Responses".to_owned(),
        })]
    );
    assert_eq!(
        message.usage,
        Some(TokenUsage {
            input_tokens: 11,
            output_tokens: 4,
            total_tokens: 15,
            cached_input_tokens: Some(3),
            reasoning_tokens: Some(1),
        })
    );
}

#[test]
fn parallel_function_deltas_finish_in_output_index_order() {
    let events = feed_sse(FUNCTION_SSE).expect("function fixture");
    let ModelEvent::TurnFinished { message } = events.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert_eq!(message.finish_reason, FinishReason::ToolCalls);
    let calls = message
        .parts
        .iter()
        .map(|part| match part {
            AssistantPart::ToolCall(call) => call.id.as_str(),
            _ => panic!("expected only calls"),
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, ["call_1", "call_2"]);
}

#[test]
fn reasoning_refusal_and_incomplete_are_mapped_without_new_part_types() {
    let reasoning = feed_sse(concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_r\",\"model\":\"gpt-test\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"output_index\":0,\"summary_index\":0,\"delta\":\"Checked\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_1\",\"output_index\":0,\"summary_index\":0,\"text\":\"Checked\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Checked\"}]}}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_r\",\"model\":\"gpt-test\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
    ))
    .expect("reasoning fixture");
    let ModelEvent::TurnFinished { message } = reasoning.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert_eq!(message.finish_reason, FinishReason::Length);
    assert!(matches!(
        message.parts.as_slice(),
        [AssistantPart::Reasoning(_)]
    ));

    let refusal = feed_sse(concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_f\",\"model\":\"gpt-test\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg_f\",\"type\":\"message\",\"content\":[]}}\n\n",
        "data: {\"type\":\"response.refusal.done\",\"item_id\":\"msg_f\",\"output_index\":0,\"content_index\":0,\"refusal\":\"Cannot comply\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"msg_f\",\"type\":\"message\",\"content\":[{\"type\":\"refusal\",\"refusal\":\"Cannot comply\"}]}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_f\",\"model\":\"gpt-test\"}}\n\n",
    ))
    .expect("refusal fixture");
    let ModelEvent::TurnFinished { message } = refusal.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert_eq!(message.finish_reason, FinishReason::ContentFilter);
}

#[test]
fn deepseek_opaque_reasoning_round_trips_only_on_the_exact_route() {
    let exact = bound_adapter(
        ResponsesProtocolAdapter::deepseek(),
        "https://api.deepseek.com/v1/",
        "deepseek-v4-pro",
    );
    let events = feed_sse_with_adapter(
        concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_ds\",\"model\":\"deepseek-v4-pro\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_ds\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_ds\",\"type\":\"reasoning\",\"status\":\"completed\",\"summary\":[],\"content\":[{\"type\":\"reasoning_text\",\"text\":\"Need the tool\"}],\"encrypted_content\":\"cipher-marker\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ds\",\"model\":\"deepseek-v4-pro\"}}\n\n",
        ),
        exact.clone(),
        "deepseek-v4-pro",
    )
    .expect("DeepSeek reasoning fixture");
    let ModelEvent::TurnFinished { message } = events.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert_eq!(message.parts.len(), 2);
    let AssistantPart::Reasoning(reasoning) = &message.parts[0] else {
        panic!("expected normalized reasoning")
    };
    assert_eq!(reasoning.id.as_str(), "rs_ds:content:0");
    assert_eq!(reasoning.text, "Need the tool");
    let AssistantPart::ProviderState(state) = &message.parts[1] else {
        panic!("expected opaque provider state")
    };
    assert_eq!(state.related_part_id(), Some(&reasoning.id));
    assert_eq!(
        state.route_fingerprint(),
        exact.route_fingerprint.as_deref()
    );
    assert!(!format!("{state:?}").contains("cipher-marker"));

    let history = request(vec![
        user("user_ds_1", "use a tool"),
        ConversationMessage::Assistant(message.clone()),
        user("user_ds_2", "continue"),
    ]);
    let exact_value = serde_json::to_value(
        encode_request_with_images(
            &history,
            &agent_model::PreparedModelImages::default(),
            &exact,
            "deepseek-v4-pro",
        )
        .expect("exact replay"),
    )
    .expect("request JSON");
    let reasoning_items = exact_value["input"]
        .as_array()
        .expect("input")
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .collect::<Vec<_>>();
    assert_eq!(reasoning_items.len(), 1);
    assert_eq!(reasoning_items[0]["encrypted_content"], "cipher-marker");
    assert_eq!(
        exact_value["include"],
        json!(["reasoning.encrypted_content"])
    );

    let switched = bound_adapter(
        ResponsesProtocolAdapter::deepseek(),
        "https://api.deepseek.com/another-v1",
        "deepseek-v4-pro",
    );
    let switched_value = serde_json::to_value(
        encode_request_with_images(
            &history,
            &agent_model::PreparedModelImages::default(),
            &switched,
            "deepseek-v4-pro",
        )
        .expect("cross-route normalized replay"),
    )
    .expect("request JSON");
    let reasoning_items = switched_value["input"]
        .as_array()
        .expect("input")
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .collect::<Vec<_>>();
    assert_eq!(reasoning_items.len(), 1);
    assert!(reasoning_items[0].get("encrypted_content").is_none());
    assert!(reasoning_items[0].get("id").is_none());
    assert!(reasoning_items[0].get("summary").is_none());
    assert_eq!(
        reasoning_items[0]["content"][0],
        json!({"type":"reasoning_text","text":"Need the tool"})
    );
}

#[test]
fn kimi_opaque_reasoning_round_trips_on_the_exact_route() {
    let exact = bound_adapter(
        ResponsesProtocolAdapter::kimi(),
        "https://api.kimi.com/coding/v1",
        "k3",
    );
    let events = feed_sse_with_adapter(
        concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_k3\",\"model\":\"k3\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_k3\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.reasoning_summary_part.added\",\"item_id\":\"rs_k3\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_k3\",\"output_index\":0,\"summary_index\":0,\"delta\":\"Checked\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_k3\",\"output_index\":0,\"summary_index\":0,\"text\":\"Checked\"}\n\n",
            "data: {\"type\":\"response.reasoning_summary_part.done\",\"item_id\":\"rs_k3\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"Checked\"}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_k3\",\"type\":\"reasoning\",\"status\":\"completed\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Checked\"}],\"encrypted_content\":\"k3-cipher\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_k3\",\"model\":\"k3\"}}\n\n",
        ),
        exact.clone(),
        "k3",
    )
    .expect("Kimi encrypted reasoning fixture");
    let ModelEvent::TurnFinished { message } = events.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert!(matches!(
        message.parts.as_slice(),
        [AssistantPart::Reasoning(reasoning), AssistantPart::ProviderState(state)]
            if reasoning.text == "Checked" && state.related_part_id() == Some(&reasoning.id)
    ));

    let history = request(vec![
        user("user_k3_1", "continue"),
        ConversationMessage::Assistant(message.clone()),
        user("user_k3_2", "continue again"),
    ]);
    let value = serde_json::to_value(
        encode_request_with_images(
            &history,
            &agent_model::PreparedModelImages::default(),
            &exact,
            "k3",
        )
        .expect("exact Kimi replay"),
    )
    .expect("request JSON");
    let reasoning = value["input"]
        .as_array()
        .expect("input")
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("reasoning item");
    assert_eq!(reasoning["encrypted_content"], "k3-cipher");
    assert_eq!(value["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn deepseek_canonicalizes_interleaved_chat_parts_before_tool_outputs() {
    let history = request(vec![
        user("user_chat_1", "review"),
        ConversationMessage::Assistant(AssistantMessage {
            id: message_id("assistant_chat_1"),
            model: ModelIdentity::new(provider("deepseek"), "deepseek-v4-flash"),
            parts: vec![
                AssistantPart::ToolCall(ToolCall {
                    id: call_id("call_chat_1"),
                    name: tool_name("read_file"),
                    arguments: json!({"path":"one.rs"}),
                }),
                AssistantPart::Reasoning(ReasoningPart {
                    id: part_id("part_1"),
                    text: "Checked the files".to_owned(),
                }),
                AssistantPart::Text(TextPart {
                    id: part_id("part_2"),
                    text: "Review complete".to_owned(),
                }),
                AssistantPart::ToolCall(ToolCall {
                    id: call_id("call_chat_2"),
                    name: tool_name("read_file"),
                    arguments: json!({"path":"two.rs"}),
                }),
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_chat_1"),
            result: ToolResult {
                call_id: call_id("call_chat_1"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("one"),
                metadata: None,
            },
        }),
        ConversationMessage::Tool(ToolMessage {
            id: message_id("tool_chat_2"),
            result: ToolResult {
                call_id: call_id("call_chat_2"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("two"),
                metadata: None,
            },
        }),
        user("user_chat_2", "continue"),
    ]);
    let deepseek = bound_adapter(
        ResponsesProtocolAdapter::deepseek(),
        "https://api.deepseek.com/v1",
        "deepseek-v4-flash",
    );
    let value = serde_json::to_value(
        encode_request_with_images(
            &history,
            &agent_model::PreparedModelImages::default(),
            &deepseek,
            "deepseek-v4-flash",
        )
        .expect("encode migrated Chat history"),
    )
    .expect("request JSON");
    let input = value["input"].as_array().expect("input");
    let reasoning = &input[1];

    assert!(reasoning.get("id").is_none());
    assert!(reasoning.get("summary").is_none());
    assert!(reasoning.get("encrypted_content").is_none());
    assert_eq!(
        reasoning["content"][0],
        json!({"type":"reasoning_text","text":"Checked the files"})
    );
    assert_eq!(input[2]["role"], "assistant");
    assert_eq!(input[2]["content"][0]["text"], "Review complete");
    assert_eq!(input[3]["type"], "function_call");
    assert_eq!(input[3]["call_id"], "call_chat_1");
    assert_eq!(input[4]["type"], "function_call");
    assert_eq!(input[4]["call_id"], "call_chat_2");
    assert_eq!(input[5]["type"], "function_call_output");
    assert_eq!(input[5]["call_id"], "call_chat_1");
    assert_eq!(input[6]["type"], "function_call_output");
    assert_eq!(input[6]["call_id"], "call_chat_2");
    assert_eq!(input[7]["role"], "user");
}

#[test]
fn deepseek_reasoning_accepts_generic_content_part_boundaries() {
    let deepseek = bound_adapter(
        ResponsesProtocolAdapter::deepseek(),
        "https://api.deepseek.com/v1",
        "deepseek-v4-flash",
    );
    let events = feed_sse_with_adapter(
        concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_ds_parts\",\"model\":\"deepseek-v4-flash\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_parts\",\"type\":\"reasoning\",\"summary\":[],\"content\":[]}}\n\n",
            "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"item_id\":\"rs_parts\",\"content_index\":0,\"part\":{\"type\":\"reasoning_text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"item_id\":\"rs_parts\",\"content_index\":0,\"delta\":\"Checked\"}\n\n",
            "data: {\"type\":\"response.reasoning_text.done\",\"output_index\":0,\"item_id\":\"rs_parts\",\"content_index\":0,\"text\":\"Checked\"}\n\n",
            "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"item_id\":\"rs_parts\",\"content_index\":0,\"part\":{\"type\":\"reasoning_text\",\"text\":\"Checked\"}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_parts\",\"type\":\"reasoning\",\"summary\":[],\"content\":[{\"type\":\"reasoning_text\",\"text\":\"Checked\"}],\"encrypted_content\":\"cipher\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ds_parts\",\"model\":\"deepseek-v4-flash\"}}\n\n",
        ),
        deepseek,
        "deepseek-v4-flash",
    )
    .expect("DeepSeek content-part reasoning fixture");
    let ModelEvent::TurnFinished { message } = events.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert!(matches!(
        message.parts.as_slice(),
        [AssistantPart::Reasoning(reasoning), AssistantPart::ProviderState(_)]
            if reasoning.text == "Checked"
    ));
}

#[test]
fn compatible_corrupt_state_fails_while_null_encrypted_content_stays_normalized() {
    let exact = bound_adapter(
        ResponsesProtocolAdapter::deepseek(),
        "https://api.deepseek.com/v1",
        "deepseek-v4-pro",
    );
    let corrupt = OpaqueProviderState::new_routed(
        provider("deepseek"),
        exact.protocol.clone(),
        "responses.reasoning_item",
        "application/json",
        1,
        part_id("rs_bad:content:0"),
        exact.route_fingerprint.clone().expect("bound route"),
        b"not-json".to_vec(),
    )
    .expect("bounded corrupt state");
    let history = request(vec![
        user("user_bad", "continue"),
        ConversationMessage::Assistant(AssistantMessage {
            id: message_id("assistant_bad"),
            model: ModelIdentity::new(provider("deepseek"), "deepseek-v4-pro"),
            parts: vec![
                AssistantPart::Reasoning(ReasoningPart {
                    id: part_id("rs_bad:content:0"),
                    text: "reason".to_owned(),
                }),
                AssistantPart::ProviderState(corrupt),
            ],
            finish_reason: FinishReason::Stop,
            usage: None,
        }),
    ]);
    assert!(matches!(
        encode_request_with_images(
            &history,
            &agent_model::PreparedModelImages::default(),
            &exact,
            "deepseek-v4-pro",
        ),
        Err(ModelError::Protocol(_))
    ));

    let qwen = bound_adapter(
        ResponsesProtocolAdapter::qwen(),
        "https://dashscope.example/compatible-mode/v1",
        "qwen3.8-max",
    );
    let events = feed_sse_with_adapter(
        concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_q\",\"model\":\"qwen3.8-max\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_q\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_q\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Checked\"}],\"encrypted_content\":null}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_q\",\"model\":\"qwen3.8-max\"}}\n\n",
        ),
        qwen,
        "qwen3.8-max",
    )
    .expect("Qwen summary fixture");
    let ModelEvent::TurnFinished { message } = events.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert!(matches!(
        message.parts.as_slice(),
        [AssistantPart::Reasoning(_)]
    ));
}

#[test]
fn qwen_reasoning_text_stream_and_final_summary_form_one_part() {
    let events = feed_sse_with_adapter(
        concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_q_stream\",\"model\":\"qwen3.8-max\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_q_stream\",\"type\":\"reasoning\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"item_id\":\"rs_q_stream\",\"content_index\":0,\"delta\":\"Checked\"}\n\n",
            "data: {\"type\":\"response.reasoning_text.done\",\"output_index\":0,\"item_id\":\"rs_q_stream\",\"content_index\":0,\"text\":\"Checked\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_q_stream\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Checked\"}],\"content\":[],\"encrypted_content\":null}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_q_stream\",\"model\":\"qwen3.8-max\"}}\n\n",
        ),
        ResponsesProtocolAdapter::qwen(),
        "qwen3.8-max",
    )
    .expect("Qwen reasoning stream fixture");

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelEvent::ReasoningStarted { .. }))
            .count(),
        1
    );
    let ModelEvent::TurnFinished { message } = events.last().expect("terminal") else {
        panic!("expected completed turn")
    };
    assert!(matches!(
        message.parts.as_slice(),
        [AssistantPart::Reasoning(reasoning)]
            if reasoning.id.as_str() == "rs_q_stream:summary:0"
                && reasoning.text == "Checked"
    ));
}

#[test]
fn malformed_streams_fail_closed() {
    let mut assembler = ResponsesAssembler::new(adapter(), "fixture-model".to_owned());
    assembler
        .push(&json!({"type":"response.created","response":{"id":"resp_bad","model":"m"}}))
        .expect("created");
    assert!(matches!(
        assembler
            .push(&json!({
                "type":"response.output_item.added",
                "output_index":0,
                "item":{"id":"hosted_1","type":"web_search_call"}
            }))
            .expect_err("unknown executable item"),
        ModelError::Protocol(_)
    ));

    let mut missing_done = ResponsesAssembler::new(adapter(), "fixture-model".to_owned());
    missing_done
        .push(&json!({"type":"response.created","response":{"id":"resp_open","model":"m"}}))
        .expect("created");
    missing_done
        .push(&json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"id":"msg_open","type":"message","content":[]}
        }))
        .expect("item");
    assert!(matches!(
        missing_done
            .push(&json!({"type":"response.completed","response":{"id":"resp_open"}}))
            .expect_err("missing item done"),
        ModelError::Protocol(_)
    ));

    let mut bad_args = ResponsesAssembler::new(adapter(), "fixture-model".to_owned());
    bad_args
        .push(&json!({"type":"response.created","response":{"id":"resp_args","model":"m"}}))
        .expect("created");
    bad_args
        .push(&json!({
            "type":"response.output_item.added","output_index":0,
            "item":{"id":"fc_bad","type":"function_call","call_id":"call_bad","name":"tool","arguments":""}
        }))
        .expect("item");
    assert!(matches!(
        bad_args
            .push(&json!({
                "type":"response.function_call_arguments.done","output_index":0,
                "item_id":"fc_bad","arguments":"{"
            }))
            .expect_err("bad arguments"),
        ModelError::ToolArguments(_)
    ));
}

#[test]
fn non_streaming_decode_reuses_the_item_state_machine() {
    let response = json!({
        "id": "resp_sync",
        "model": "gpt-test",
        "status": "completed",
        "output": [{
            "id": "msg_sync",
            "type": "message",
            "role": "assistant",
            "content": [{"type":"output_text","text":"sync","annotations":[]}]
        }],
        "usage": {"input_tokens":1,"output_tokens":1,"total_tokens":2}
    });
    let events = decode_response(&response, &adapter(), "fixture-model").expect("decode");
    assert!(matches!(
        events.last(),
        Some(ModelEvent::TurnFinished { .. })
    ));
}

#[tokio::test]
async fn service_posts_to_responses_and_keeps_chat_transport_independent() {
    let transport = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200, TEXT_SSE,
    ))]));
    let service = OpenAiResponsesService::with_transport(
        "https://api.openai.test/v1",
        BearerCredential::new("secret"),
        "fixture-model",
        128_000,
        adapter(),
        transport.clone(),
    )
    .expect("service");
    let stream = service
        .stream(
            request(vec![user("user_1", "hello")]),
            ModelCallContext::default(),
        )
        .await
        .expect("established");
    let collected = EventCollector::collect_validated(stream).await;
    assert_eq!(
        collected.assert_finished().finish_reason,
        FinishReason::Stop
    );
    let requests = transport.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://api.openai.test/v1/responses");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn service_turns_midstream_transport_and_post_terminal_data_into_one_failure() {
    let interrupted = Arc::new(RecordedTransport::new([Ok(RecordedResponse::chunked(
        200,
        vec![
            BodyStep::Chunk(
                b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_i\",\"model\":\"m\"}}\n\n"
                    .to_vec(),
            ),
            BodyStep::Fail("reset".to_owned()),
        ],
    ))]));
    let service = OpenAiResponsesService::with_transport(
        "https://api.openai.test/v1",
        BearerCredential::new("secret"),
        "fixture-model",
        128_000,
        adapter(),
        interrupted,
    )
    .expect("service");
    let stream = service
        .stream(
            request(vec![user("user_i", "hello")]),
            ModelCallContext::default(),
        )
        .await
        .expect("established");
    let collected = EventCollector::collect_validated(stream).await;
    assert!(matches!(
        collected.assert_failed(),
        ModelError::Transport { .. }
    ));
    assert_eq!(collected.terminals().count(), 1);

    let duplicate = Arc::new(RecordedTransport::new([Ok(RecordedResponse::new(
        200,
        concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_d\",\"model\":\"m\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_d\",\"model\":\"m\"}}\n\n",
            "data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"resp_d\",\"model\":\"m\"}}\n\n",
        ),
    ))]));
    let service = OpenAiResponsesService::with_transport(
        "https://api.openai.test/v1",
        BearerCredential::new("secret"),
        "fixture-model",
        128_000,
        adapter(),
        duplicate,
    )
    .expect("service");
    let stream = service
        .stream(
            request(vec![user("user_d", "hello")]),
            ModelCallContext::default(),
        )
        .await
        .expect("established");
    let collected = EventCollector::collect_validated(stream).await;
    assert!(matches!(collected.assert_failed(), ModelError::Protocol(_)));
    assert_eq!(collected.terminals().count(), 1);
}
