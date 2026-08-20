//! OpenAI Chat Completions 协议方言、Schema、Codec 与服务实现。

mod adapter;
mod decode;
mod encode;
mod schema;
mod service;

#[cfg(test)]
mod tests;

pub use adapter::{ChatProtocolAdapter, ReasoningReplayPolicy};
pub use decode::{ChunkAssembler, decode_assistant_message, decode_error_body, decode_response};
pub use encode::encode_request;
pub(crate) use encode::encode_request_with_images;
pub use schema::{
    ChatAssistantMessage, ChatChunk, ChatChunkChoice, ChatChunkDelta, ChatCompletionTokensDetails,
    ChatContentPart, ChatErrorBody, ChatErrorDetail, ChatFunctionDefinition, ChatMessage,
    ChatNamedToolChoice, ChatNamedToolChoiceFunction, ChatPromptTokensDetails, ChatRequest,
    ChatResponse, ChatResponseChoice, ChatStreamOptions, ChatSystemMessage, ChatTool, ChatToolCall,
    ChatToolCallDelta, ChatToolCallFunction, ChatToolCallFunctionDelta, ChatToolChoice,
    ChatToolChoiceMode, ChatToolKind, ChatToolMessage, ChatUsage, ChatUserContent, ChatUserMessage,
};
pub use service::{OpenAiChatCompletionsService, OpenAiChatCompletionsServiceError};
