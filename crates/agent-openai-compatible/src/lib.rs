//! OpenAI-compatible 模型协议适配器。
//!
//! Chat Completions 与 Responses 使用独立的 Adapter、Schema、Codec、流状态机和 Service；
//! 只共享 credential、endpoint、Transport、SSE parser、工具 Schema 降级和图片 Data URL 等
//! 协议无关基础设施。协议由 Runtime 配置显式选择，Service 不执行失败回退。
//!
//! Responses 首版由 [`ResponsesProtocolAdapter`] 与 [`OpenAiResponsesService`] 提供，固定使用
//! 本地完整历史、`store: false` 和流式 `/responses`。Provider 精确方言与 Opaque State 在后续
//! 里程碑接入，不从兼容响应动态推断。
//!
//! 纯协议 Codec（M4）：
//!
//! - [`ChatProtocolAdapter`]：显式描述 Chat Completions Provider 方言差异（reasoning 字段名、generation 参数支持、
//!   max tokens 字段名），Codec 不判断 Provider 名称。DeepSeek 使用 `reasoning_content`；vLLM
//!   具名方言优先使用当前 `reasoning`，并兼容读取旧版 `reasoning_content`。
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
//! - [`ObservedTransport`] / [`ProviderWireEvent`]：在不改变请求、响应分块和错误的前提下，
//!   观察已排除 credential header 的原始 wire 事实；观察数据仍包含高敏正文。
//! - [`SseParser`] / [`SseFrame`]：SSE frame 增量解析。
//! - [`OpenAiChatCompletionsService`] / [`BearerCredential`]：把 Chat Codec 与共享 Transport 组装成
//!   [`agent_model::ModelService`]；credential 只进入 `Authorization` header，
//!   不出现在 Debug、事件和错误文本中。
//!
//! DeepSeek 具名方言与 thinking/tool-call 契约（M6）：
//!
//! - [`ChatProtocolAdapter::deepseek`]：DeepSeek Chat Completions 方言。thinking 开关不编码进
//!   ChatProtocolAdapter；调用方经 `ModelRequest::provider_options` 的 `deepseek` 命名空间传
//!   `{"thinking": {"type": "enabled"}}`，编码器按命名空间合并进请求根。thinking
//!   模式拒绝显式 `tool_choice`，`Auto` 由编码器统一省略（与线上默认值等价），
//!   其余显式选择在编码期返回 `Config`。
//! - `fixtures/chat/deepseek/`：两轮 thinking + tool call 的可审阅 SSE transcript 与预期
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
//!    cargo test -p agent-openai-compatible deepseek -- --ignored --nocapture
//!    ```
//!
//! smoke test 绝不打印 credential 与请求/响应原文，也绝不把真实响应写入 fixture；
//! 失败时区分协议回归与外部网络/额度问题。

mod chat;
mod responses;
mod shared;

pub use chat::{
    ChatAssistantMessage, ChatChunk, ChatChunkChoice, ChatChunkDelta, ChatCompletionTokensDetails,
    ChatContentPart, ChatErrorBody, ChatErrorDetail, ChatFunctionDefinition, ChatMessage,
    ChatNamedToolChoice, ChatNamedToolChoiceFunction, ChatPromptTokensDetails, ChatProtocolAdapter,
    ChatRequest, ChatResponse, ChatResponseChoice, ChatStreamOptions, ChatSystemMessage, ChatTool,
    ChatToolCall, ChatToolCallDelta, ChatToolCallFunction, ChatToolCallFunctionDelta,
    ChatToolChoice, ChatToolChoiceMode, ChatToolKind, ChatToolMessage, ChatUsage, ChatUserContent,
    ChatUserMessage, ChunkAssembler, OpenAiChatCompletionsService,
    OpenAiChatCompletionsServiceError, ReasoningReplayPolicy, decode_assistant_message,
    decode_error_body, decode_response, encode_request,
};
pub use responses::{
    FunctionOutputShape, OpenAiResponsesService, OpenAiResponsesServiceError,
    ResponsesProtocolAdapter, decode_response as decode_responses_response,
};
pub use shared::{
    BearerCredential, BodyStream, ObservedTransport, ProviderWireEvent, ProviderWireObserver,
    RecordedWireRequest, ReqwestTransport, SseFrame, SseParser, ToolSchemaDialect, Transport,
    TransportError, TransportFuture, TransportRequest, TransportResponse, TransportTimeouts,
};
