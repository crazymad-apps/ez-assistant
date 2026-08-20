use std::collections::BTreeMap;

use agent_model::{ModelError, ModelRequest, ReasoningEffort, ToolImageProjection};
use agent_types::{
    AssistantPart, ConversationMessage, MAX_PROVIDER_STATE_TURN_BYTES, PartId, ToolChoice,
    ToolResultPart, UserPart,
};
use serde_json::Value;

use crate::shared::{encode_tool_schema, image_data_url};

use super::{
    ResponsesProtocolAdapter,
    adapter::FunctionOutputShape,
    schema::{
        ResponsesContent, ResponsesFunctionOutput, ResponsesFunctionTool, ResponsesInputItem,
        ResponsesReasoningConfig, ResponsesRequest, ResponsesRole, ResponsesToolChoice,
        ResponsesToolChoiceMode,
    },
};

const CONTEXT_SUMMARY_PREFIX: &str = "[Context summary derived from earlier conversation]";
const TOOL_RESULT_IMAGE_PLACEHOLDER_VERSION: &str = "tool_result_image";
const RESERVED_REQUEST_KEYS: &[&str] = &[
    "model",
    "instructions",
    "input",
    "tools",
    "tool_choice",
    "temperature",
    "top_p",
    "max_output_tokens",
    "reasoning",
    "include",
    "store",
    "stream",
    "previous_response_id",
    "conversation",
    "background",
];

pub(super) fn encode_request_with_images(
    request: &ModelRequest,
    prepared_images: &agent_model::PreparedModelImages,
    adapter: &ResponsesProtocolAdapter,
    model: &str,
) -> Result<ResponsesRequest, ModelError> {
    request
        .conversation
        .validate_tool_exchange_pairs()
        .map_err(|error| ModelError::Config(error.to_string()))?;

    let mut input = Vec::new();
    let mut index = 0_usize;
    while index < request.conversation.messages.len() {
        let message = &request.conversation.messages[index];
        match message {
            ConversationMessage::System(message) => input.push(message_item(
                ResponsesRole::System,
                ResponsesContent::InputText {
                    text: message.text.clone(),
                },
            )),
            ConversationMessage::ContextSummary(message) => input.push(message_item(
                ResponsesRole::System,
                ResponsesContent::InputText {
                    text: format!("{CONTEXT_SUMMARY_PREFIX}\n{}", message.text),
                },
            )),
            ConversationMessage::User(message) => {
                input.push(encode_user_message(message, prepared_images));
            }
            ConversationMessage::Assistant(message) => {
                encode_assistant_message(message, &mut input, adapter)?;
                let has_tool_calls = message
                    .parts
                    .iter()
                    .any(|part| matches!(part, AssistantPart::ToolCall(_)));
                index += 1;
                if !has_tool_calls {
                    continue;
                }
                let mut image_envelope = Vec::new();
                while let Some(ConversationMessage::Tool(tool)) =
                    request.conversation.messages.get(index)
                {
                    let (item, images) = encode_tool_result(tool, prepared_images, adapter)?;
                    input.push(item);
                    image_envelope.extend(images);
                    index += 1;
                }
                if !image_envelope.is_empty() {
                    input.push(ResponsesInputItem::Message {
                        role: ResponsesRole::User,
                        content: image_envelope,
                    });
                }
                continue;
            }
            ConversationMessage::Tool(_) => {
                return Err(ModelError::Config(
                    "function output must be encoded with its complete function-call batch"
                        .to_owned(),
                ));
            }
        }
        index += 1;
    }

    let tools = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|definition| {
                    Ok(ResponsesFunctionTool {
                        kind: "function",
                        name: definition.name.as_str().to_owned(),
                        description: definition.description.clone(),
                        parameters: encode_tool_schema(
                            &definition.input_schema,
                            adapter.tool_schema_dialect,
                        )
                        .map_err(|error| {
                            ModelError::Config(format!(
                                "tool `{}` has an incompatible input schema: {error}",
                                definition.name.as_str()
                            ))
                        })?,
                        strict: false,
                    })
                })
                .collect::<Result<Vec<_>, ModelError>>()?,
        )
    };
    let tool_choice = encode_tool_choice(&request.tool_choice, &request.tools, adapter)?;

    let generation = &request.generation;
    let temperature = match generation.temperature {
        Some(value) if adapter.supports_temperature => Some(value),
        Some(_) => {
            return Err(ModelError::Config(
                "generation.temperature is not supported by this Responses route".to_owned(),
            ));
        }
        None => None,
    };
    let top_p = match generation.top_p {
        Some(value) if adapter.supports_top_p => Some(value),
        Some(_) => {
            return Err(ModelError::Config(
                "generation.top_p is not supported by this Responses route".to_owned(),
            ));
        }
        None => None,
    };
    let max_output_tokens = match generation.max_output_tokens {
        Some(value) if adapter.supports_max_output_tokens => Some(value),
        Some(_) => {
            return Err(ModelError::Config(
                "generation.max_output_tokens is not supported by this Responses route".to_owned(),
            ));
        }
        None => None,
    };

    let mut extra = BTreeMap::new();
    if !generation.stop.is_empty() {
        if !adapter.supports_stop {
            return Err(ModelError::Config(
                "generation.stop is not supported by this Responses route".to_owned(),
            ));
        }
        extra.insert("stop".to_owned(), serde_json::json!(generation.stop));
    }
    let reasoning = request
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.effort.as_ref())
        .map(|effort| ResponsesReasoningConfig {
            effort: adapter
                .reasoning_effort_values
                .get(effort)
                .cloned()
                .unwrap_or_else(|| Value::String(reasoning_effort_name(effort).to_owned())),
        });
    if let Some(options) = request.provider_options.get(adapter.provider.as_str()) {
        let Value::Object(options) = options else {
            return Err(ModelError::Config(
                "Responses provider options must be a JSON object".to_owned(),
            ));
        };
        for (key, value) in options {
            if RESERVED_REQUEST_KEYS.contains(&key.as_str()) || extra.contains_key(key) {
                return Err(ModelError::Config(format!(
                    "provider option `{key}` collides with a reserved Responses request field"
                )));
            }
            extra.insert(key.clone(), value.clone());
        }
    }

    Ok(ResponsesRequest {
        model: model.to_owned(),
        instructions: (!request.system.parts().is_empty())
            .then(|| request.system.parts().join("\n\n")),
        input,
        tools,
        tool_choice,
        temperature,
        top_p,
        max_output_tokens,
        reasoning,
        include: adapter
            .include_encrypted_reasoning
            .then(|| vec!["reasoning.encrypted_content".to_owned()]),
        store: false,
        stream: true,
        extra,
    })
}

fn message_item(role: ResponsesRole, content: ResponsesContent) -> ResponsesInputItem {
    ResponsesInputItem::Message {
        role,
        content: vec![content],
    }
}

fn encode_user_message(
    message: &agent_types::UserMessage,
    prepared_images: &agent_model::PreparedModelImages,
) -> ResponsesInputItem {
    let mut content = Vec::new();
    for part in &message.parts {
        match part {
            UserPart::Text(text) | UserPart::Injected(text) => {
                content.push(ResponsesContent::InputText {
                    text: text.text.clone(),
                });
            }
            UserPart::FileReferences(files) => {
                let mut ordinary = Vec::new();
                for file in &files.files {
                    if let Some(image) = prepared_images.get_file_reference(&file.readable_path) {
                        flush_file_references(&mut content, &mut ordinary);
                        content.push(ResponsesContent::InputImage {
                            image_url: image_data_url(image),
                        });
                    } else {
                        ordinary.push(file.clone());
                    }
                }
                flush_file_references(&mut content, &mut ordinary);
            }
        }
    }
    ResponsesInputItem::Message {
        role: ResponsesRole::User,
        content,
    }
}

fn encode_assistant_message(
    message: &agent_types::AssistantMessage,
    input: &mut Vec<ResponsesInputItem>,
    adapter: &ResponsesProtocolAdapter,
) -> Result<(), ModelError> {
    let reasoning_parts = message
        .parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Reasoning(part) => Some((part.id.clone(), part.text.as_str())),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let mut opaque_items = BTreeMap::new();
    let mut opaque_reasoning_parts = BTreeMap::new();
    let mut turn_payload_bytes = 0_usize;
    for (part_index, part) in message.parts.iter().enumerate() {
        let AssistantPart::ProviderState(state) = part else {
            continue;
        };
        let Some(route_fingerprint) = adapter.route_fingerprint.as_deref() else {
            continue;
        };
        if state.provider() != &adapter.provider
            || state.protocol() != &adapter.protocol
            || state.route_fingerprint() != Some(route_fingerprint)
        {
            continue;
        }
        if adapter.opaque_reasoning != super::adapter::OpaqueReasoningPolicy::PreserveEncryptedItem
            || state.state_type() != "responses.reasoning_item"
            || state.media_type() != "application/json"
            || state.format_version() != 1
        {
            return Err(ModelError::Config(
                "compatible Responses provider state uses an unsupported format".to_owned(),
            ));
        }
        turn_payload_bytes = turn_payload_bytes
            .checked_add(state.payload().len())
            .ok_or_else(|| ModelError::Protocol("provider state byte count overflow".to_owned()))?;
        if turn_payload_bytes > MAX_PROVIDER_STATE_TURN_BYTES {
            return Err(ModelError::Protocol(
                "Responses provider state exceeds the per-turn byte limit".to_owned(),
            ));
        }
        let related_part_id = state.related_part_id().ok_or_else(|| {
            ModelError::Protocol(
                "compatible Responses provider state has no related part".to_owned(),
            )
        })?;
        let (item, raw_parts) = decode_opaque_reasoning_item(state.payload())?;
        if !raw_parts.iter().any(|(id, _)| id == related_part_id) {
            return Err(ModelError::Protocol(
                "Responses provider state is not bound to one of its reasoning parts".to_owned(),
            ));
        }
        for (id, text) in raw_parts {
            if reasoning_parts.get(&id).copied() != Some(text.as_str()) {
                return Err(ModelError::Protocol(
                    "Responses provider state contradicts its normalized reasoning part".to_owned(),
                ));
            }
            if opaque_reasoning_parts.insert(id, part_index).is_some() {
                return Err(ModelError::Protocol(
                    "multiple Responses provider states bind the same reasoning part".to_owned(),
                ));
            }
        }
        opaque_items.insert(part_index, item);
    }

    for (part_index, part) in message.parts.iter().enumerate() {
        match part {
            AssistantPart::Reasoning(part) if opaque_reasoning_parts.contains_key(&part.id) => {}
            AssistantPart::Reasoning(part) => input.push(ResponsesInputItem::Reasoning {
                id: part.id.as_str().to_owned(),
                summary: serde_json::json!([{
                    "type": "summary_text",
                    "text": part.text,
                }]),
                content: None,
                encrypted_content: None,
                extra: BTreeMap::new(),
            }),
            AssistantPart::Text(part) => input.push(message_item(
                ResponsesRole::Assistant,
                ResponsesContent::OutputText {
                    text: part.text.clone(),
                },
            )),
            AssistantPart::ToolCall(call) => input.push(ResponsesInputItem::FunctionCall {
                call_id: call.id.as_str().to_owned(),
                name: call.name.as_str().to_owned(),
                arguments: serde_json::to_string(&call.arguments).map_err(|error| {
                    ModelError::ToolArguments(format!(
                        "tool call `{}` arguments cannot be serialized: {error}",
                        call.id
                    ))
                })?,
            }),
            AssistantPart::ProviderState(_) => {
                if let Some(item) = opaque_items.remove(&part_index) {
                    input.push(item);
                }
            }
        }
    }
    Ok(())
}

fn decode_opaque_reasoning_item(
    payload: &[u8],
) -> Result<(ResponsesInputItem, Vec<(PartId, String)>), ModelError> {
    let value: Value = serde_json::from_slice(payload).map_err(|_| {
        ModelError::Protocol("Responses provider state is not valid JSON".to_owned())
    })?;
    let Value::Object(mut object) = value else {
        return Err(ModelError::Protocol(
            "Responses provider state is not a JSON object".to_owned(),
        ));
    };
    if object
        .remove("type")
        .and_then(|value| value.as_str().map(str::to_owned))
        .as_deref()
        != Some("reasoning")
    {
        return Err(ModelError::Protocol(
            "Responses provider state is not a reasoning item".to_owned(),
        ));
    }
    let id = object
        .remove("id")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| {
            ModelError::Protocol("reasoning provider state has no item id".to_owned())
        })?;
    let summary = object
        .remove("summary")
        .unwrap_or_else(|| serde_json::json!([]));
    let content = object.remove("content");
    let encrypted_content = object.remove("encrypted_content");
    if !encrypted_content
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        return Err(ModelError::Protocol(
            "reasoning provider state has no non-empty encrypted content".to_owned(),
        ));
    }
    let parts = normalized_reasoning_parts(&id, &summary, content.as_ref())?;
    if parts.is_empty() {
        return Err(ModelError::Protocol(
            "reasoning provider state has no normalized text part".to_owned(),
        ));
    }
    Ok((
        ResponsesInputItem::Reasoning {
            id,
            summary,
            content,
            encrypted_content,
            extra: object.into_iter().collect(),
        },
        parts,
    ))
}

fn normalized_reasoning_parts(
    item_id: &str,
    summary: &Value,
    content: Option<&Value>,
) -> Result<Vec<(PartId, String)>, ModelError> {
    let mut parts = Vec::new();
    append_reasoning_parts(&mut parts, item_id, "summary", summary, "summary_text")?;
    if let Some(content) = content {
        append_reasoning_parts(&mut parts, item_id, "content", content, "reasoning_text")?;
    }
    Ok(parts)
}

fn append_reasoning_parts(
    target: &mut Vec<(PartId, String)>,
    item_id: &str,
    segment: &str,
    value: &Value,
    expected_type: &str,
) -> Result<(), ModelError> {
    let values = value.as_array().ok_or_else(|| {
        ModelError::Protocol(format!("Responses reasoning {segment} is not an array"))
    })?;
    for (index, value) in values.iter().enumerate() {
        if value.get("type").and_then(Value::as_str) != Some(expected_type) {
            return Err(ModelError::Protocol(format!(
                "Responses reasoning {segment} has an unsupported part"
            )));
        }
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ModelError::Protocol(format!("Responses reasoning {segment} part has no text"))
            })?
            .to_owned();
        let id = PartId::new(format!("{item_id}:{segment}:{index}")).map_err(|_| {
            ModelError::Protocol("Responses reasoning part id is invalid".to_owned())
        })?;
        target.push((id, text));
    }
    Ok(())
}

fn encode_tool_result(
    message: &agent_types::ToolMessage,
    prepared_images: &agent_model::PreparedModelImages,
    adapter: &ResponsesProtocolAdapter,
) -> Result<(ResponsesInputItem, Vec<ResponsesContent>), ModelError> {
    let mut text_parts = Vec::new();
    let mut native_parts = Vec::new();
    let mut image_envelope = Vec::new();
    for (part_index, part) in message.result.content.as_parts().iter().enumerate() {
        match part {
            ToolResultPart::Text { text } => {
                text_parts.push(text.clone());
                native_parts.push(ResponsesContent::InputText { text: text.clone() });
            }
            ToolResultPart::Json { value } => {
                let text = value.to_string();
                text_parts.push(text.clone());
                native_parts.push(ResponsesContent::InputText { text });
            }
            ToolResultPart::Image { image } => {
                let prepared = prepared_images
                    .get_tool_image(image.relative_path())
                    .ok_or_else(|| {
                        ModelError::Resource(format!(
                            "tool image `{}` was not prepared",
                            image.relative_path()
                        ))
                    })?;
                match adapter.tool_image_projection {
                    ToolImageProjection::Unsupported => {
                        return Err(ModelError::Config(
                            "Responses tool result image projection is not configured".to_owned(),
                        ));
                    }
                    ToolImageProjection::NativeFunctionOutput => {
                        if adapter.function_output_shape != FunctionOutputShape::ContentParts {
                            return Err(ModelError::Config(
                                "native Responses image output requires content parts".to_owned(),
                            ));
                        }
                        native_parts.push(ResponsesContent::InputImage {
                            image_url: image_data_url(prepared),
                        });
                    }
                    ToolImageProjection::AggregatedUserInput => {
                        let label =
                            tool_image_placeholder(message.result.call_id.as_str(), part_index);
                        text_parts.push(label.clone());
                        native_parts.push(ResponsesContent::InputText {
                            text: label.clone(),
                        });
                        image_envelope.push(ResponsesContent::InputText { text: label });
                        image_envelope.push(ResponsesContent::InputImage {
                            image_url: image_data_url(prepared),
                        });
                    }
                }
            }
        }
    }
    let output = match adapter.function_output_shape {
        FunctionOutputShape::StringOnly => ResponsesFunctionOutput::Text(text_parts.join("\n")),
        FunctionOutputShape::ContentParts
            if adapter.tool_image_projection == ToolImageProjection::AggregatedUserInput =>
        {
            ResponsesFunctionOutput::Text(text_parts.join("\n"))
        }
        FunctionOutputShape::ContentParts => ResponsesFunctionOutput::Parts(native_parts),
    };
    Ok((
        ResponsesInputItem::FunctionCallOutput {
            call_id: message.result.call_id.as_str().to_owned(),
            output,
        },
        image_envelope,
    ))
}

fn encode_tool_choice(
    choice: &ToolChoice,
    tools: &[agent_types::ToolDefinition],
    adapter: &ResponsesProtocolAdapter,
) -> Result<Option<ResponsesToolChoice>, ModelError> {
    if tools.is_empty() {
        return match choice {
            ToolChoice::Auto | ToolChoice::None => Ok(None),
            ToolChoice::Required | ToolChoice::Named(_) => Err(ModelError::Config(
                "tool_choice requires at least one tool".to_owned(),
            )),
        };
    }
    match choice {
        ToolChoice::Auto if adapter.tool_choice.auto => Ok(None),
        ToolChoice::None if adapter.tool_choice.none => Ok(Some(ResponsesToolChoice::Mode(
            ResponsesToolChoiceMode::None,
        ))),
        ToolChoice::Required if adapter.tool_choice.required => Ok(Some(
            ResponsesToolChoice::Mode(ResponsesToolChoiceMode::Required),
        )),
        ToolChoice::Named(name)
            if adapter.tool_choice.named && tools.iter().any(|tool| tool.name == *name) =>
        {
            Ok(Some(ResponsesToolChoice::Function {
                kind: "function",
                name: name.as_str().to_owned(),
            }))
        }
        ToolChoice::Named(name) if !tools.iter().any(|tool| tool.name == *name) => {
            Err(ModelError::Config(format!(
                "tool_choice names `{}` which is not in the request tools",
                name.as_str()
            )))
        }
        _ => Err(ModelError::Config(
            "tool_choice is not supported by this Responses route".to_owned(),
        )),
    }
}

fn flush_file_references(
    content: &mut Vec<ResponsesContent>,
    files: &mut Vec<agent_types::FileReference>,
) {
    if files.is_empty() {
        return;
    }
    let mut xml = String::from("<attached_files>\n");
    for file in std::mem::take(files) {
        xml.push_str("  <file>\n    <name>");
        push_xml_text(&mut xml, &file.original_name);
        xml.push_str("</name>\n    <path>");
        push_xml_text(&mut xml, &file.readable_path);
        xml.push_str("</path>\n  </file>\n");
    }
    xml.push_str("</attached_files>");
    content.push(ResponsesContent::InputText { text: xml });
}

fn push_xml_text(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            character => output.push(character),
        }
    }
}

fn reasoning_effort_name(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::XHigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

fn tool_image_placeholder(call_id: &str, part_index: usize) -> String {
    let escaped = call_id
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    format!(
        "[{TOOL_RESULT_IMAGE_PLACEHOLDER_VERSION} call_id=\"{escaped}\" part_index=\"{part_index}\" supplied_in_following_batch]"
    )
}
