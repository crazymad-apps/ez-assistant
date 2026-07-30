//! OpenAI Chat Completions compatible model adapter.
//!
//! 纯协议 Codec（M4）：
//!
//! - [`Profile`]：显式描述 Provider 方言差异（reasoning 字段名、generation 参数支持、
//!   max tokens 字段名），Codec 不判断 Provider 名称。
//! - [`encode_request`]：把规范 [`agent_model::ModelRequest`] 编码为原生
//!   [`ChatRequest`]。
//! - [`ChunkAssembler`]：把流式 [`ChatChunk`] 聚合为规范事件序列。
//! - [`decode_assistant_message`] / [`decode_response`] / [`decode_error_body`]：
//!   完整消息、非流式响应与错误正文的解码。
//!
//! HTTP/SSE Transport 与流式 Adapter（M5）：
//!
//! - [`Transport`] / [`TransportRequest`] / [`TransportResponse`] / [`TransportError`]：
//!   可替换的 HTTP Transport 边界；[`ReqwestTransport`] 是默认实现。
//! - [`SseParser`] / [`SseFrame`]：SSE frame 增量解析。
//! - [`OpenAiCompatibleService`] / [`BearerCredential`]：把 Codec 与 Transport 组装成
//!   [`agent_model::ModelService`]；credential 只进入 `Authorization` header，
//!   不出现在 Debug、事件和错误文本中。
//!
//! DeepSeek 具名方言与 thinking/tool-call 契约（M6）：
//!
//! - [`Profile::deepseek`]：DeepSeek Chat Completions 方言。thinking 开关不编码进
//!   Profile；调用方经 `ModelRequest::provider_options` 的 `deepseek` 命名空间传
//!   `{"thinking": {"type": "enabled"}}`，编码器按命名空间合并进请求根。thinking
//!   模式拒绝显式 `tool_choice`，`Auto` 由编码器统一省略（与线上默认值等价），
//!   其余显式选择在编码期返回 `Config`。
//! - `fixtures/deepseek/`：两轮 thinking + tool call 的可审阅 SSE transcript 与预期
//!   请求 JSON，供离线契约测试回放，不含真实凭据与真实响应。
//!
//! ## 真实 API smoke test（默认忽略）
//!
//! 默认测试完全离线。真实 DeepSeek API smoke test 被 `#[ignore]`，只在显式开启时运行：
//!
//! 1. 在仓库根创建 `.env`（已被 gitignore）：
//!
//!    ```text
//!    DEEPSEEK_API_KEY=<在此填入真实 Key>
//!    # 可选，缺省为 https://api.deepseek.com
//!    DEEPSEEK_BASE_URL=https://api.deepseek.com
//!    # 可选，缺省为 128000
//!    DEEPSEEK_CONTEXT_WINDOW_TOKENS=128000
//!    ```
//!
//! 2. 运行：
//!
//!    ```bash
//!    cargo test -p agent-provider-openai-compatible deepseek -- --ignored --nocapture
//!    ```
//!
//! smoke test 绝不打印 credential 与请求/响应原文，也绝不把真实响应写入 fixture；
//! 失败时区分协议回归与外部网络/额度问题。

mod decode;
mod encode;
mod profile;
mod schema;
mod service;
mod sse;
mod transport;

#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod deepseek_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod transport_tests;

pub use decode::{ChunkAssembler, decode_assistant_message, decode_error_body, decode_response};
pub use encode::encode_request;
pub use profile::Profile;
pub use schema::{
    ChatAssistantMessage, ChatChunk, ChatChunkChoice, ChatChunkDelta, ChatCompletionTokensDetails,
    ChatContentPart, ChatContentPartKind, ChatErrorBody, ChatErrorDetail, ChatFunctionDefinition,
    ChatMessage, ChatNamedToolChoice, ChatNamedToolChoiceFunction, ChatPromptTokensDetails,
    ChatRequest, ChatResponse, ChatResponseChoice, ChatStreamOptions, ChatSystemMessage, ChatTool,
    ChatToolCall, ChatToolCallDelta, ChatToolCallFunction, ChatToolCallFunctionDelta,
    ChatToolChoice, ChatToolChoiceMode, ChatToolKind, ChatToolMessage, ChatUsage, ChatUserContent,
    ChatUserMessage,
};
pub use service::{BearerCredential, OpenAiCompatibleService};
pub use sse::{SseFrame, SseParser};
pub use transport::{
    BodyStream, ReqwestTransport, Transport, TransportError, TransportFuture, TransportRequest,
    TransportResponse, TransportTimeouts,
};
