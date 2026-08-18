use std::collections::BTreeMap;

use agent_model::{ModelError, ModelRequest, ReasoningEffort};
use agent_types::{
    AssistantMessage, AssistantPart, ContextSummaryMessage, ConversationMessage,
    FileReferencesPart, ToolChoice, ToolDefinition, ToolMessage, ToolResultContent, UserMessage,
    UserPart,
};
use serde_json::Value;

use crate::{
    Profile,
    schema::{
        ChatAssistantMessage, ChatContentPart, ChatContentPartKind, ChatFunctionDefinition,
        ChatMessage, ChatNamedToolChoice, ChatNamedToolChoiceFunction, ChatRequest,
        ChatStreamOptions, ChatSystemMessage, ChatTool, ChatToolCall, ChatToolCallFunction,
        ChatToolChoice, ChatToolChoiceMode, ChatToolKind, ChatToolMessage, ChatUserContent,
        ChatUserMessage,
    },
    tool_schema::encode_tool_schema,
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
/// 参数是否受支持由 [`Profile`] 决定；请求设置了 Profile 不支持的参数时返回
/// [`ModelError::Config`]，不静默丢弃。`tool_choice` 例外于"原样透传"：`Auto`
/// 是线上的默认值，统一省略（语义相同，且兼容拒绝显式 `tool_choice` 的
/// Provider）；其余显式选择在 Profile 声明 `supports_tool_choice = false` 时
/// 返回 [`ModelError::Config`]。Provider 私有选项按命名空间合并进请求根；
/// 使用保留键或与已编码字段冲突的键同样返回 [`ModelError::Config`]，不静默覆盖。
pub fn encode_request(
    request: &ModelRequest,
    profile: &Profile,
    model: &str,
) -> Result<ChatRequest, ModelError> {
    request
        .conversation
        .validate_tool_exchange_pairs()
        .map_err(|error| ModelError::Config(error.to_string()))?;
    let mut messages = Vec::new();
    // 每条 system 指令按序生成一个 system 消息，置于对话消息之前。
    for system in request.system.parts() {
        messages.push(ChatMessage::System(ChatSystemMessage {
            content: system.clone(),
        }));
    }
    for message in &request.conversation.messages {
        messages.push(encode_conversation_message(message, profile)?);
    }

    let tools = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|definition| encode_tool_definition(definition, profile))
                .collect::<Result<Vec<_>, _>>()?,
        )
    };
    // 没有工具时 `Auto`/`None` 省略即可（省略与 Provider 默认行为一致）；
    // `Required`/`Named` 与空工具列表矛盾，属于上游装配错误，显式报 Config
    // 而不是静默退化为 Provider 默认行为。`Auto` 一律省略：省略与显式传
    // `"auto"` 语义相同，同时兼容拒绝显式 tool_choice 的 Provider（如
    // DeepSeek thinking 模式，任何显式值都会 400）。`Named` 还必须指向请求
    // 工具列表中存在的名称。其余显式选择在 Profile 不支持时属于必然被拒的
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
        choice if profile.supports_tool_choice => Some(encode_tool_choice(choice)),
        _ => {
            return Err(ModelError::Config(
                "tool_choice is set but the profile does not support explicit tool choice"
                    .to_owned(),
            ));
        }
    };

    let generation = &request.generation;
    let temperature = match generation.temperature {
        Some(value) if profile.supports_temperature => Some(value),
        Some(_) => {
            return Err(ModelError::Config(
                "generation.temperature is set but the profile does not support it".to_owned(),
            ));
        }
        None => None,
    };
    let top_p = match generation.top_p {
        Some(value) if profile.supports_top_p => Some(value),
        Some(_) => {
            return Err(ModelError::Config(
                "generation.top_p is set but the profile does not support it".to_owned(),
            ));
        }
        None => None,
    };
    let stop = if generation.stop.is_empty() {
        None
    } else if profile.supports_stop {
        Some(generation.stop.clone())
    } else {
        return Err(ModelError::Config(
            "generation.stop is set but the profile does not support it".to_owned(),
        ));
    };

    let mut extra = BTreeMap::new();
    if let Some(max_output_tokens) = generation.max_output_tokens {
        let Some(field) = &profile.max_output_tokens_field else {
            return Err(ModelError::Config(
                "generation.max_output_tokens is set but the profile declares no max tokens field"
                    .to_owned(),
            ));
        };
        extra.insert(field.clone(), Value::from(u64::from(max_output_tokens)));
    }
    if let Some(reasoning) = &request.reasoning
        && let Some(effort) = &reasoning.effort
    {
        let Some(field) = &profile.reasoning_effort_field else {
            return Err(ModelError::Config(
                "reasoning effort is set but the profile declares no reasoning effort field"
                    .to_owned(),
            ));
        };
        let effort = match effort {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        };
        extra.insert(field.clone(), Value::String(effort.to_owned()));
    }
    // 只合并命名空间等于本 Profile Provider 的私有选项对象，其他命名空间忽略。
    // 私有选项不得使用请求根保留键（flatten 后会产生重复 JSON 字段），也不得
    // 覆盖已编码的动态字段（max_tokens 等）；两类冲突都在编码期显式失败，
    // 不静默覆盖。
    if let Some(options) = request.provider_options.get(profile.provider.as_str())
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
    profile: &Profile,
) -> Result<ChatMessage, ModelError> {
    match message {
        ConversationMessage::System(message) => Ok(ChatMessage::System(ChatSystemMessage {
            content: message.text.clone(),
        })),
        ConversationMessage::ContextSummary(message) => {
            Ok(ChatMessage::System(encode_context_summary(message)))
        }
        ConversationMessage::User(message) => Ok(ChatMessage::User(encode_user_message(message))),
        ConversationMessage::Assistant(message) => Ok(ChatMessage::Assistant(
            encode_assistant_message(message, profile)?,
        )),
        ConversationMessage::Tool(message) => Ok(ChatMessage::Tool(encode_tool_message(message))),
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
fn encode_user_message(message: &UserMessage) -> ChatUserMessage {
    let texts: Vec<String> = message
        .parts
        .iter()
        .map(|part| match part {
            UserPart::Text(text) | UserPart::Injected(text) => text.text.clone(),
            UserPart::FileReferences(files) => render_file_references(files),
        })
        .collect();
    let content = match texts.len() {
        0 | 1 => ChatUserContent::Text(texts.into_iter().next().unwrap_or_default()),
        _ => ChatUserContent::Parts(
            texts
                .into_iter()
                .map(|text| ChatContentPart {
                    kind: ChatContentPartKind::Text,
                    text,
                })
                .collect(),
        ),
    };
    ChatUserMessage { content }
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
/// Reasoning parts 按序拼接进 Profile 声明的 reasoning 字段；Text parts 按序拼接为
/// content；ToolCall parts 按序进入 `tool_calls`。规范 Conversation 可能由另一个
/// Provider 方言产生；目标 Profile 未声明 reasoning 字段时，reasoning 作为不可移植的
/// 辅助内容在 wire 投影中省略，正文和工具交换仍按规范回放。`ProviderState` 不具备这种
/// 通用降级语义，仍然显式失败。
///
/// Profile 声明 [`Profile::tool_calls_require_reasoning`] 时（DeepSeek thinking 模式：
/// 带 tool calls 的 assistant 消息必须在后续请求中回传 `reasoning_content`，
/// 见 <https://api-docs.deepseek.com/guides/thinking_mode/>），编码器会对 Provider 偶发的
/// 缺失内容补一个仅用于线上协议的单空格。规范消息仍如实表示为
/// “没有 reasoning part”，不向 UI 或 Journal 伪造 reasoning 内容。
fn encode_assistant_message(
    message: &AssistantMessage,
    profile: &Profile,
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
    let reasoning_to_encode = if !reasoning_text.is_empty() {
        Some(reasoning_text.as_str())
    } else if profile.tool_calls_require_reasoning && !tool_calls.is_empty() {
        Some(MISSING_REASONING_WIRE_PLACEHOLDER)
    } else {
        None
    };
    if let Some(reasoning) = reasoning_to_encode
        && let Some(field) = &profile.reasoning_content_field
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
fn encode_tool_message(message: &ToolMessage) -> ChatToolMessage {
    let content = match &message.result.content {
        ToolResultContent::Text(text) => text.clone(),
        // Chat Completions 的 tool content 只接受字符串；JSON 结果序列化后回传，
        // 结构对模型仍然完整可见。
        ToolResultContent::Json(value) => value.to_string(),
    };
    ChatToolMessage {
        tool_call_id: message.result.call_id.as_str().to_owned(),
        content,
    }
}

/// 把规范工具定义编码为原生 function 工具。
fn encode_tool_definition(
    definition: &ToolDefinition,
    profile: &Profile,
) -> Result<ChatTool, ModelError> {
    let parameters = encode_tool_schema(&definition.input_schema, profile.tool_schema_dialect)
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
