use std::collections::BTreeMap;

use agent_model::{
    ModelError, ModelRequest, ReasoningEffort, ToolImageProjection,
    plan_tool_result_image_envelope, tool_result_image_label,
};
use agent_types::{
    AssistantMessage, AssistantPart, ContextInsertionPayload, ContextSummaryMessage,
    ConversationMessage, FileReferencesPart, ToolChoice, ToolDefinition, ToolMessage,
    ToolResultPart, UserMessage, UserPart,
};
use serde_json::Value;

use crate::shared::{encode_tool_schema, image_data_url};

use super::{
    ChatProtocolAdapter,
    schema::{
        ChatAssistantMessage, ChatContentPart, ChatFunctionDefinition, ChatImageUrl, ChatMessage,
        ChatNamedToolChoice, ChatNamedToolChoiceFunction, ChatRequest, ChatStreamOptions,
        ChatSystemMessage, ChatTool, ChatToolCall, ChatToolCallFunction, ChatToolChoice,
        ChatToolChoiceMode, ChatToolKind, ChatToolMessage, ChatUserContent, ChatUserMessage,
    },
};

/// 请求根的保留字段名。Provider 私有选项经 flatten 平铺到请求根，使用这些键
/// 会产生重复 JSON 字段或覆盖规范字段，编码期一律拒绝（见 `encode_request`）。
const RESERVED_REQUEST_KEYS: &[&str] = &[
    "model",
    "messages",
    "tools",
    "tool_choice",
    "temperature",
    "top_p",
    "stop",
    "stream",
    "stream_options",
];

/// 派生摘要在线上编码为 system message 时使用的固定说明。
const CONTEXT_SUMMARY_PREFIX: &str = "[Context summary derived from earlier conversation]";

/// DeepSeek thinking 回放要求 `reasoning_content` 字段存在，但模型偶尔会
/// 在 tool-call 轮次省略该字段。单空格只是 wire 占位：它不进入规范消息，
/// 同时兼容会拒绝空字符串的 DeepSeek 模型。
const MISSING_REASONING_WIRE_PLACEHOLDER: &str = " ";

/// 把规范请求编码为 Chat Completions 原生请求。
///
/// 请求固定为流式（`stream: true` 且要求流末下发 usage）：Adapter 只走流式调用，
/// 非流式响应结构仅为 fixture、回读和 [`crate::decode_response`] 服务。模型名是
/// 服务实例的构造期绑定，由调用方随 `model` 参数传入，不在规范请求中。
///
/// 参数是否受支持由 [`ChatProtocolAdapter`] 决定；请求设置了 ChatProtocolAdapter 不支持的参数时返回
/// [`ModelError::Config`]，不静默丢弃。`tool_choice` 例外于"原样透传"：`Auto`
/// 是线上的默认值，统一省略（语义相同，且兼容拒绝显式 `tool_choice` 的
/// Provider）；其余显式选择在 ChatProtocolAdapter 声明 `supports_tool_choice = false` 时
/// 返回 [`ModelError::Config`]。Provider 私有选项按命名空间合并进请求根；
/// 使用保留键或与已编码字段冲突的键同样返回 [`ModelError::Config`]，不静默覆盖。
pub fn encode_request(
    request: &ModelRequest,
    adapter: &ChatProtocolAdapter,
    model: &str,
) -> Result<ChatRequest, ModelError> {
    encode_request_with_images(
        request,
        &agent_model::PreparedModelImages::default(),
        adapter,
        model,
    )
}

pub(crate) fn encode_request_with_images(
    request: &ModelRequest,
    prepared_images: &agent_model::PreparedModelImages,
    adapter: &ChatProtocolAdapter,
    model: &str,
) -> Result<ChatRequest, ModelError> {
    request
        .conversation
        .validate_tool_exchange_pairs()
        .map_err(|error| ModelError::Config(error.to_string()))?;
    let mut messages = Vec::new();
    let mut leading_system_parts = request.system.parts().to_vec();
    let mut index = 0_usize;
    while let Some(message) = request.conversation.messages.get(index) {
        let content = match message {
            ConversationMessage::System(message) => message.text.clone(),
            ConversationMessage::ContextSummary(message) => encode_context_summary(message).content,
            _ => break,
        };
        leading_system_parts.push(content);
        index += 1;
    }
    // 规范 Snapshot 继续保留独立 Part；Chat wire 合并为唯一前导 system，兼容只允许
    // messages[0] 为 system 的严格模板，同时保持 Part 与前导对话事实的冻结顺序。
    if !leading_system_parts.is_empty() {
        messages.push(ChatMessage::System(ChatSystemMessage {
            content: leading_system_parts.join("\n\n"),
        }));
    }
    while index < request.conversation.messages.len() {
        let message = &request.conversation.messages[index];
        messages.push(encode_conversation_message(
            message,
            adapter,
            prepared_images,
        )?);
        index += 1;
        let ConversationMessage::Assistant(assistant) = message else {
            continue;
        };
        if !assistant
            .parts
            .iter()
            .any(|part| matches!(part, AssistantPart::ToolCall(_)))
        {
            continue;
        }

        let mut tool_messages = Vec::new();
        while let Some(ConversationMessage::Tool(tool)) = request.conversation.messages.get(index) {
            messages.push(ChatMessage::Tool(encode_tool_message(tool, adapter)?));
            tool_messages.push(tool);
            index += 1;
        }
        if let Some(plan) = plan_tool_result_image_envelope(assistant, &tool_messages) {
            let image_envelope = encode_tool_image_envelope(&plan.payload, prepared_images)?;
            messages.push(ChatMessage::User(ChatUserMessage {
                content: ChatUserContent::Parts(image_envelope),
            }));
        }
    }

    let tools = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|definition| encode_tool_definition(definition, adapter))
                .collect::<Result<Vec<_>, _>>()?,
        )
    };
    // 没有工具时 `Auto`/`None` 省略即可（省略与 Provider 默认行为一致）；
    // `Required`/`Named` 与空工具列表矛盾，属于上游装配错误，显式报 Config
    // 而不是静默退化为 Provider 默认行为。`Auto` 一律省略：省略与显式传
    // `"auto"` 语义相同，同时兼容拒绝显式 tool_choice 的 Provider（如
    // DeepSeek thinking 模式，任何显式值都会 400）。`Named` 还必须指向请求
    // 工具列表中存在的名称。其余显式选择在 ChatProtocolAdapter 不支持时属于必然被拒的
    // 请求，编码期直接报 Config。
    let tool_choice = match &request.tool_choice {
        ToolChoice::Auto => None,
        ToolChoice::None if tools.is_none() => None,
        ToolChoice::Required if tools.is_none() => {
            return Err(ModelError::Config(
                "tool_choice is required but the request carries no tools".to_owned(),
            ));
        }
        ToolChoice::Named(name) if tools.is_none() => {
            return Err(ModelError::Config(format!(
                "tool_choice names `{}` but the request carries no tools",
                name.as_str()
            )));
        }
        ToolChoice::Named(name) if !request.tools.iter().any(|tool| &tool.name == name) => {
            return Err(ModelError::Config(format!(
                "tool_choice names `{}` which is not in the request tools",
                name.as_str()
            )));
        }
        choice if adapter.supports_tool_choice => Some(encode_tool_choice(choice)),
        _ => {
            return Err(ModelError::Config(
                "tool_choice is set but the adapter does not support explicit tool choice"
                    .to_owned(),
            ));
        }
    };

    let generation = &request.generation;
    let temperature = match generation.temperature {
        Some(value) if adapter.supports_temperature => Some(value),
        Some(_) => {
            return Err(ModelError::Config(
                "generation.temperature is set but the adapter does not support it".to_owned(),
            ));
        }
        None => None,
    };
    let top_p = match generation.top_p {
        Some(value) if adapter.supports_top_p => Some(value),
        Some(_) => {
            return Err(ModelError::Config(
                "generation.top_p is set but the adapter does not support it".to_owned(),
            ));
        }
        None => None,
    };
    let stop = if generation.stop.is_empty() {
        None
    } else if adapter.supports_stop {
        Some(generation.stop.clone())
    } else {
        return Err(ModelError::Config(
            "generation.stop is set but the adapter does not support it".to_owned(),
        ));
    };

    let mut extra = BTreeMap::new();
    if let Some(max_output_tokens) = generation.max_output_tokens {
        let Some(field) = &adapter.max_output_tokens_field else {
            return Err(ModelError::Config(
                "generation.max_output_tokens is set but the adapter declares no max tokens field"
                    .to_owned(),
            ));
        };
        extra.insert(field.clone(), Value::from(u64::from(max_output_tokens)));
    }
    if let Some(reasoning) = &request.reasoning
        && let Some(effort) = &reasoning.effort
    {
        let Some(field) = &adapter.reasoning_effort_field else {
            return Err(ModelError::Config(
                "reasoning effort is set but the adapter declares no reasoning effort field"
                    .to_owned(),
            ));
        };
        let value = adapter
            .reasoning_effort_values
            .get(effort)
            .cloned()
            .unwrap_or_else(|| {
                Value::String(
                    match effort {
                        ReasoningEffort::Low => "low",
                        ReasoningEffort::Medium => "medium",
                        ReasoningEffort::High => "high",
                        ReasoningEffort::XHigh => "xhigh",
                        ReasoningEffort::Max => "max",
                    }
                    .to_owned(),
                )
            });
        extra.insert(field.clone(), value);
    }
    // 只合并命名空间等于本 ChatProtocolAdapter Provider 的私有选项对象，其他命名空间忽略。
    // 私有选项不得使用请求根保留键（flatten 后会产生重复 JSON 字段），也不得
    // 覆盖已编码的动态字段（max_tokens 等）；两类冲突都在编码期显式失败，
    // 不静默覆盖。
    if let Some(options) = request.provider_options.get(adapter.provider.as_str())
        && let Value::Object(options) = options
    {
        for (key, value) in options {
            if RESERVED_REQUEST_KEYS.contains(&key.as_str()) {
                return Err(ModelError::Config(format!(
                    "provider option `{key}` collides with a reserved request field"
                )));
            }
            if extra.contains_key(key) {
                return Err(ModelError::Config(format!(
                    "provider option `{key}` collides with an encoded request field"
                )));
            }
            extra.insert(key.clone(), value.clone());
        }
    }

    Ok(ChatRequest {
        model: model.to_owned(),
        messages,
        tools,
        tool_choice,
        temperature,
        top_p,
        stop,
        stream: true,
        stream_options: Some(ChatStreamOptions {
            include_usage: true,
        }),
        extra,
    })
}

/// 把一条规范对话消息编码为原生消息。
fn encode_conversation_message(
    message: &ConversationMessage,
    adapter: &ChatProtocolAdapter,
    prepared_images: &agent_model::PreparedModelImages,
) -> Result<ChatMessage, ModelError> {
    match message {
        ConversationMessage::System(message) => Ok(ChatMessage::System(ChatSystemMessage {
            content: message.text.clone(),
        })),
        ConversationMessage::ContextSummary(message) => {
            Ok(ChatMessage::System(encode_context_summary(message)))
        }
        ConversationMessage::User(message) => Ok(ChatMessage::User(encode_user_message(
            message,
            prepared_images,
        ))),
        ConversationMessage::Assistant(message) => Ok(ChatMessage::Assistant(
            encode_assistant_message(message, adapter)?,
        )),
        ConversationMessage::Tool(_) => Err(ModelError::Config(
            "tool result must be encoded with its complete tool-call batch".to_owned(),
        )),
    }
}

/// 把派生上下文摘要编码为带固定说明的 system 消息。
fn encode_context_summary(message: &ContextSummaryMessage) -> ChatSystemMessage {
    ChatSystemMessage {
        content: format!("{CONTEXT_SUMMARY_PREFIX}\n{}", message.text),
    }
}

/// 把规范 user 消息编码为原生 user 消息。
///
/// 三种规范 Part 都编码为文本；File References 先渲染为稳定 XML。
/// 恰好一个 part 时用纯字符串，多个 parts 时用 text part 数组保序。
fn encode_user_message(
    message: &UserMessage,
    prepared_images: &agent_model::PreparedModelImages,
) -> ChatUserMessage {
    let mut parts = Vec::new();
    for part in &message.parts {
        match part {
            UserPart::Text(text) | UserPart::Injected(text) => {
                parts.push(ChatContentPart::Text {
                    text: text.text.clone(),
                });
            }
            UserPart::InternalContext(context) => {
                parts.push(ChatContentPart::Text {
                    text: context.text.clone(),
                });
            }
            UserPart::QuotedText(quoted) => {
                parts.push(ChatContentPart::Text {
                    text: crate::shared::render_quoted_text(quoted),
                });
            }
            UserPart::FileReferences(files) => {
                let mut ordinary = Vec::new();
                for file in &files.files {
                    if let Some(image) = prepared_images.get_file_reference(&file.readable_path) {
                        flush_file_references(&mut parts, files, &mut ordinary);
                        parts.push(ChatContentPart::ImageUrl {
                            image_url: ChatImageUrl {
                                url: image_data_url(image),
                            },
                        });
                    } else {
                        ordinary.push(file.clone());
                    }
                }
                flush_file_references(&mut parts, files, &mut ordinary);
            }
        }
    }
    let content = match parts.as_slice() {
        [] => ChatUserContent::Text(String::new()),
        [ChatContentPart::Text { text }] => ChatUserContent::Text(text.clone()),
        _ => ChatUserContent::Parts(parts),
    };
    ChatUserMessage { content }
}

fn flush_file_references(
    parts: &mut Vec<ChatContentPart>,
    source: &FileReferencesPart,
    files: &mut Vec<agent_types::FileReference>,
) {
    if files.is_empty() {
        return;
    }
    parts.push(ChatContentPart::Text {
        text: render_file_references(&FileReferencesPart {
            id: source.id.clone(),
            files: std::mem::take(files),
        }),
    });
}

/// 文件引用只声明 Agent 可读文件，不伪造已读取文件或 Tool Result。
fn render_file_references(part: &FileReferencesPart) -> String {
    let mut xml = String::from("<attached_files>\n");
    for file in &part.files {
        xml.push_str("  <file>\n    <name>");
        push_xml_text(&mut xml, &file.original_name);
        xml.push_str("</name>\n    <path>");
        push_xml_text(&mut xml, &file.readable_path);
        xml.push_str("</path>\n  </file>\n");
    }
    xml.push_str("</attached_files>");
    xml
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

/// 把规范 assistant 消息编码为原生 assistant 消息。
///
/// Reasoning parts 按序拼接进 ChatProtocolAdapter 声明的 reasoning 字段；Text parts 按序拼接为
/// content；ToolCall parts 按序进入 `tool_calls`。规范 Conversation 可能由另一个
/// Provider 方言产生；目标 ChatProtocolAdapter 未声明 reasoning 字段时，reasoning 作为不可移植的
/// 辅助内容在 wire 投影中省略，正文和工具交换仍按规范回放。`ProviderState` 不具备这种
/// 通用降级语义，仍然显式失败。
///
/// ChatProtocolAdapter 声明 [`ChatProtocolAdapter::tool_calls_require_reasoning`] 时（DeepSeek thinking 模式：
/// 带 tool calls 的 assistant 消息必须在后续请求中回传 `reasoning_content`，
/// 见 <https://api-docs.deepseek.com/guides/thinking_mode/>），编码器会对 Provider 偶发的
/// 缺失内容补一个仅用于线上协议的单空格。规范消息仍如实表示为
/// “没有 reasoning part”，不向 UI 或 Journal 伪造 reasoning 内容。
fn encode_assistant_message(
    message: &AssistantMessage,
    adapter: &ChatProtocolAdapter,
) -> Result<ChatAssistantMessage, ModelError> {
    let mut reasoning_text = String::new();
    let mut content_text = String::new();
    let mut tool_calls = Vec::new();
    for part in &message.parts {
        match part {
            AssistantPart::Reasoning(part) => reasoning_text.push_str(&part.text),
            AssistantPart::Text(part) => content_text.push_str(&part.text),
            AssistantPart::ToolCall(call) => {
                let arguments = serde_json::to_string(&call.arguments).map_err(|error| {
                    ModelError::ToolArguments(format!(
                        "tool call `{}` arguments cannot be serialized: {error}",
                        call.id
                    ))
                })?;
                tool_calls.push(ChatToolCall {
                    id: call.id.as_str().to_owned(),
                    kind: ChatToolKind::Function,
                    function: ChatToolCallFunction {
                        name: call.name.as_str().to_owned(),
                        arguments,
                    },
                });
            }
            // M4 没有 Provider State 映射器；显式失败而不是静默丢弃。
            AssistantPart::ProviderState(state) => {
                return Err(ModelError::Config(format!(
                    "assistant message contains provider state `{}` that this codec cannot map",
                    state.state_type()
                )));
            }
        }
    }

    let mut extra = BTreeMap::new();
    let reasoning_to_encode = match adapter.reasoning_replay {
        crate::ReasoningReplayPolicy::Drop => None,
        crate::ReasoningReplayPolicy::ToolCallsOnly if tool_calls.is_empty() => None,
        crate::ReasoningReplayPolicy::ToolCallsOnly => Some(if reasoning_text.is_empty() {
            MISSING_REASONING_WIRE_PLACEHOLDER
        } else {
            reasoning_text.as_str()
        }),
        crate::ReasoningReplayPolicy::PreserveAll if !reasoning_text.is_empty() => {
            Some(reasoning_text.as_str())
        }
        crate::ReasoningReplayPolicy::PreserveAll => None,
    };
    if let Some(reasoning) = reasoning_to_encode
        && let Some(field) = &adapter.reasoning_replay_field
    {
        extra.insert(field.clone(), Value::String(reasoning.to_owned()));
    }

    Ok(ChatAssistantMessage {
        role: None,
        content: Some(content_text),
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        extra,
    })
}

/// 把规范 tool 结果消息编码为原生 tool 消息，回填 `tool_call_id`。
fn encode_tool_message(
    message: &ToolMessage,
    adapter: &ChatProtocolAdapter,
) -> Result<ChatToolMessage, ModelError> {
    let mut encoded_parts = Vec::with_capacity(message.result.content.as_parts().len());
    for (part_index, part) in message.result.content.as_parts().iter().enumerate() {
        match part {
            ToolResultPart::Text { text } => encoded_parts.push(text.clone()),
            // Chat Completions 的 tool content 只接受字符串；JSON 结果序列化后回传，
            // 结构对模型仍然完整可见。
            ToolResultPart::Json { value } => encoded_parts.push(value.to_string()),
            ToolResultPart::Image { .. } => {
                if adapter.tool_image_projection != ToolImageProjection::AggregatedUserInput {
                    return Err(ModelError::Config(
                        "Chat tool result image projection is not configured".to_owned(),
                    ));
                }
                encoded_parts.push(tool_result_image_label(
                    message.result.call_id.as_str(),
                    part_index,
                ));
            }
        }
    }
    Ok(ChatToolMessage {
        tool_call_id: message.result.call_id.as_str().to_owned(),
        content: encoded_parts.join("\n"),
    })
}

fn encode_tool_image_envelope(
    payload: &ContextInsertionPayload,
    prepared_images: &agent_model::PreparedModelImages,
) -> Result<Vec<ChatContentPart>, ModelError> {
    let ContextInsertionPayload::ToolResultImages(images) = payload else {
        return Err(ModelError::Config(
            "tool image insertion plan has an incompatible payload".to_owned(),
        ));
    };
    let mut envelope = Vec::with_capacity(images.len().saturating_mul(2));
    for image in images {
        let prepared = prepared_images
            .get_tool_image(image.image.relative_path())
            .ok_or_else(|| {
                ModelError::Resource(format!(
                    "tool image `{}` was not prepared",
                    image.image.relative_path()
                ))
            })?;
        envelope.push(ChatContentPart::Text {
            text: image.label.clone(),
        });
        envelope.push(ChatContentPart::ImageUrl {
            image_url: ChatImageUrl {
                url: image_data_url(prepared),
            },
        });
    }
    Ok(envelope)
}

/// 把规范工具定义编码为原生 function 工具。
fn encode_tool_definition(
    definition: &ToolDefinition,
    adapter: &ChatProtocolAdapter,
) -> Result<ChatTool, ModelError> {
    let parameters = encode_tool_schema(&definition.input_schema, adapter.tool_schema_dialect)
        .map_err(|error| {
            ModelError::Config(format!(
                "tool `{}` has an incompatible input schema: {error}",
                definition.name.as_str()
            ))
        })?;
    Ok(ChatTool {
        kind: ChatToolKind::Function,
        function: ChatFunctionDefinition {
            name: definition.name.as_str().to_owned(),
            description: definition.description.clone(),
            parameters,
        },
    })
}

/// 把规范工具选择策略编码为原生 `tool_choice`。
fn encode_tool_choice(choice: &ToolChoice) -> ChatToolChoice {
    match choice {
        ToolChoice::Auto => ChatToolChoice::Mode(ChatToolChoiceMode::Auto),
        ToolChoice::None => ChatToolChoice::Mode(ChatToolChoiceMode::None),
        ToolChoice::Required => ChatToolChoice::Mode(ChatToolChoiceMode::Required),
        ToolChoice::Named(name) => ChatToolChoice::Named(ChatNamedToolChoice {
            kind: ChatToolKind::Function,
            function: ChatNamedToolChoiceFunction {
                name: name.as_str().to_owned(),
            },
        }),
    }
}
