//! Agent Core 使用的 Provider-neutral（与模型厂商无关）领域类型。
//!
//! 这个 crate 只保存“数据长什么样”，不负责网络请求、工具执行或会话调度。
//! 上层代码只需要依赖这些统一类型，不需要直接使用 OpenAI、DeepSeek 等厂商的 JSON 结构。
//!
//! 最小使用示例见各模块内联测试：`conversation.rs` 演示规范消息组装，
//! `id.rs` 演示标识构造与校验。

mod conversation;
mod id;
mod model;
mod tool;

// `pub use` 把子模块中的公共类型重新导出到 crate 根部。
// 调用方因此可以写 `agent_types::AssistantMessage`，不用关心它位于哪个源码文件。
pub use conversation::{
    AssistantMessage, AssistantPart, ContextSummaryMessage, ConversationMessage,
    ConversationSnapshot, ConversationValidationError, FileReference, FileReferencesPart,
    MAX_PROVIDER_STATE_ITEM_BYTES, MAX_PROVIDER_STATE_TURN_BYTES, OpaqueProviderState,
    ProviderStateError, ReasoningPart, SystemMessage, TextPart, ToolCall, ToolMessage, UserMessage,
    UserPart,
};
pub use id::{IdentifierError, MessageId, PartId, ProtocolId, ProviderId, ToolCallId};
pub use model::{FinishReason, ModelIdentity, TokenUsage};
pub use tool::{
    ToolChoice, ToolDefinition, ToolExecutionMetadata, ToolImageReference, ToolImageReferenceError,
    ToolName, ToolNameError, ToolResult, ToolResultContent, ToolResultContentError, ToolResultPart,
    ToolResultStatus,
};
