use agent_model::{
    GenerationConfig, ModelError, ModelEvent, ModelRequest, ProviderOptions, ReasoningConfig,
    ReasoningEffort, SystemPromptSnapshot,
};
use agent_types::{
    AssistantMessage, AssistantPart, ContextSummaryMessage, ConversationMessage,
    ConversationSnapshot, FileReference, FileReferencesPart, FinishReason, MessageId,
    ModelIdentity, OpaqueProviderState, PartId, ProtocolId, ProviderId, ReasoningPart,
    SystemMessage, TextPart, TokenUsage, ToolCall, ToolCallId, ToolChoice, ToolDefinition,
    ToolMessage, ToolName, ToolResult, ToolResultContent, ToolResultStatus, UserMessage, UserPart,
};
use serde_json::{Value, json};

use crate::{
    ChatAssistantMessage, ChatChunk, ChatErrorBody, ChatMessage, ChatRequest, ChatResponse,
    ChatStreamOptions, ChunkAssembler, Profile, decode_assistant_message, decode_error_body,
    decode_response, encode_request,
};

fn provider_id(value: &str) -> ProviderId {
    ProviderId::new(value).expect("valid provider id")
}

fn protocol_id(value: &str) -> ProtocolId {
    ProtocolId::new(value).expect("valid protocol id")
}

fn message_id(value: &str) -> MessageId {
    MessageId::new(value).expect("valid message id")
}

fn part_id(value: &str) -> PartId {
    PartId::new(value).expect("valid part id")
}

fn call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid call id")
}

fn tool_name(value: &str) -> ToolName {
    ToolName::new(value).expect("valid tool name")
}

/// 基础方言：不支持 reasoning。
fn base_profile() -> Profile {
    Profile::openai_compatible(provider_id("openai"))
}

/// 带 reasoning 字段的方言（用字面量构造，DeepSeek 形态；具名构造见 [`Profile::deepseek`]）。
fn reasoning_profile() -> Profile {
    Profile {
        provider: provider_id("deepseek"),
        protocol: protocol_id("openai.chat_completions"),
        reasoning_content_field: Some("reasoning_content".to_owned()),
        reasoning_effort_field: Some("reasoning_effort".to_owned()),
        supports_temperature: true,
        supports_top_p: true,
        supports_stop: true,
        max_output_tokens_field: Some("max_tokens".to_owned()),
        supports_tool_choice: true,
        tool_calls_require_reasoning: false,
        cached_input_tokens_field: None,
    }
}

/// 所有 generation 参数都不支持的严格方言。
fn limited_profile() -> Profile {
    Profile {
        provider: provider_id("strict"),
        protocol: protocol_id("openai.chat_completions"),
        reasoning_content_field: None,
        reasoning_effort_field: None,
        supports_temperature: false,
        supports_top_p: false,
        supports_stop: false,
        max_output_tokens_field: None,
        supports_tool_choice: false,
        tool_calls_require_reasoning: false,
        cached_input_tokens_field: None,
    }
}

/// 测试请求的目标模型名；线上模型名由服务构造期绑定，编码时显式传入。
const MODEL: &str = "deepseek-reasoner";

fn request(conversation: Vec<ConversationMessage>) -> ModelRequest {
    ModelRequest {
        system: SystemPromptSnapshot::default(),
        conversation: ConversationSnapshot::new(conversation),
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        generation: GenerationConfig::default(),
        reasoning: None,
        provider_options: ProviderOptions::new(),
    }
}

fn user_message(id: &str, texts: &[&str]) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        id: message_id(id),
        parts: texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                UserPart::Text(TextPart {
                    id: part_id(&format!("text_{}", index + 1)),
                    text: (*text).to_owned(),
                })
            })
            .collect(),
    })
}

fn assistant_message(parts: Vec<AssistantPart>) -> ConversationMessage {
    ConversationMessage::Assistant(AssistantMessage {
        id: message_id("turn_1"),
        model: ModelIdentity::new(provider_id("deepseek"), "deepseek-reasoner"),
        parts,
        finish_reason: FinishReason::Stop,
        usage: None,
    })
}

fn reasoning_part(text: &str) -> AssistantPart {
    AssistantPart::Reasoning(ReasoningPart {
        id: part_id("reasoning_1"),
        text: text.to_owned(),
    })
}

fn text_part(text: &str) -> AssistantPart {
    AssistantPart::Text(TextPart {
        id: part_id("text_1"),
        text: text.to_owned(),
    })
}

fn tool_call_part(id: &str, name: &str, arguments: Value) -> AssistantPart {
    AssistantPart::ToolCall(ToolCall {
        id: call_id(id),
        name: tool_name(name),
        arguments,
    })
}

fn model_identity() -> ModelIdentity {
    ModelIdentity::new(provider_id("deepseek"), "deepseek-reasoner")
}

fn chunk(value: Value) -> ChatChunk {
    serde_json::from_value(value).expect("chunk fixture must parse")
}

fn content_chunk(content: &str) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {"content": content}}],
    }))
}

fn reasoning_chunk(text: &str) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {"reasoning_content": text}}],
    }))
}

fn tool_chunk(tool_calls: Value) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {"tool_calls": tool_calls}}],
    }))
}

fn finish_chunk(reason: &str) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
    }))
}

fn usage_only_chunk(usage: Value) -> ChatChunk {
    chunk(json!({
        "id": "chatcmpl_1",
        "model": "deepseek-reasoner",
        "choices": [],
        "usage": usage,
    }))
}

fn feed(
    assembler: &mut ChunkAssembler,
    chunks: Vec<ChatChunk>,
) -> Result<Vec<ModelEvent>, ModelError> {
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(assembler.push_chunk(&chunk)?);
    }
    Ok(events)
}

fn assemble(chunks: Vec<ChatChunk>) -> Result<Vec<ModelEvent>, ModelError> {
    let mut assembler = ChunkAssembler::new(reasoning_profile());
    let mut events = feed(&mut assembler, chunks)?;
    events.extend(assembler.finalize()?);
    Ok(events)
}

fn assemble_with_profile(
    profile: Profile,
    chunks: Vec<ChatChunk>,
) -> Result<Vec<ModelEvent>, ModelError> {
    let mut assembler = ChunkAssembler::new(profile);
    let mut events = feed(&mut assembler, chunks)?;
    events.extend(assembler.finalize()?);
    Ok(events)
}

mod decode;
mod encode;
mod round_trip;
mod stream;
