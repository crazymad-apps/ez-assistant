//! 把 Codec 与 Transport 组装成 [`ModelService`] 的流式 Adapter。
//!
//! 调用形状：
//!
//! 1. 建立前：编码请求 → 发出 HTTP 请求 → 非 2xx 读取采样错误正文并分类；
//!    这些阶段的失败（含取消）都由 `stream` 返回 `Err`。
//! 2. 2xx：SSE frame 逐个喂给 [`ChunkAssembler`]，事件逐个转发；`[DONE]` 或
//!    字节流结束后 `finalize` 产出唯一终态；所有流中失败（含取消）都以唯一
//!    `TurnFailed` 受控结束。
//! 3. 返回的事件流外包 [`LifecycleValidator`]，对调用方强制执行规范生命周期。

use std::sync::Arc;

use agent_model::{
    LifecycleValidator, ModelCallContext, ModelCapabilities, ModelError, ModelEvent,
    ModelEventStream, ModelRequest, ModelService, ModelStreamFuture, ModelTransportErrorKind,
    ToolChoiceCapabilities, ToolImageProjection,
};
use futures_core::Stream;
use futures_util::StreamExt;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::shared::{join_endpoint, validate_base_url};
use crate::{
    BearerCredential, BodyStream, ReqwestTransport, SseParser, Transport, TransportError,
    TransportRequest, TransportTimeouts,
};

use super::{
    ChatChunk, ChatErrorBody, ChatProtocolAdapter, ChunkAssembler, decode_error_body,
    encode_request_with_images,
};

/// 非 2xx 响应错误正文的采样上限（字节）；错误分类不读取完整正文。
const ERROR_BODY_SAMPLE_LIMIT: usize = 2048;

#[derive(Debug, Error)]
/// OpenAI-compatible Service 的构造错误。
///
/// 无效 URL 的错误只描述违反的规则，不回显可能含 credential 的原始输入。
pub enum OpenAiChatCompletionsServiceError {
    /// Base URL 不是可安全记录的绝对 URL。
    #[error("invalid OpenAI-compatible base URL: {0}")]
    InvalidBaseUrl(&'static str),
    /// 默认 HTTP Transport 构造失败。
    #[error("failed to construct OpenAI-compatible transport: {0}")]
    Transport(#[from] TransportError),
}

/// OpenAI Chat Completions compatible 的单次模型 Turn 服务。
///
/// 构造时注入 base URL、credential、模型名、[`ChatProtocolAdapter`] 与 [`Transport`]——一个
/// 服务实例就是一条编译完成的模型配置，调用目标在此固定，请求不再携带路由信息。
/// credential 只在发请求时写入 `Authorization` header，不进入 Debug、事件和错误文本。
pub struct OpenAiChatCompletionsService {
    /// Provider API 的 base URL；请求发往 `{base_url}/chat/completions`。
    base_url: String,
    /// 注入的 Bearer credential。
    credential: BearerCredential,
    /// Provider 侧的模型名称，例如 `deepseek-v4-flash`。
    model: String,
    /// 调用方为当前模型配置的上下文窗口上限。
    context_window_tokens: u64,
    /// Provider 方言配置。
    adapter: ChatProtocolAdapter,
    /// 底层 HTTP Transport。
    transport: Arc<dyn Transport>,
    /// 由 ChatProtocolAdapter 推导的能力声明。
    capabilities: ModelCapabilities,
}

impl OpenAiChatCompletionsService {
    /// 用默认 [`ReqwestTransport`] 和显式上下文窗口创建服务。
    pub fn new(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ChatProtocolAdapter,
        timeouts: TransportTimeouts,
    ) -> Result<Self, OpenAiChatCompletionsServiceError> {
        let base_url = validate_base_url(base_url.into())
            .map_err(OpenAiChatCompletionsServiceError::InvalidBaseUrl)?;
        let transport = ReqwestTransport::with_timeouts(timeouts)?;
        Ok(Self::build(
            base_url,
            credential,
            model,
            context_window_tokens,
            adapter,
            Arc::new(transport),
            None,
        ))
    }

    /// 使用上层唯一能力编译结果创建服务；协议 Adapter 仍负责 wire 约束。
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_capabilities(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ChatProtocolAdapter,
        capabilities: ModelCapabilities,
        timeouts: TransportTimeouts,
    ) -> Result<Self, OpenAiChatCompletionsServiceError> {
        let base_url = validate_base_url(base_url.into())
            .map_err(OpenAiChatCompletionsServiceError::InvalidBaseUrl)?;
        let transport = ReqwestTransport::with_timeouts(timeouts)?;
        Ok(Self::build(
            base_url,
            credential,
            model,
            context_window_tokens,
            adapter,
            Arc::new(transport),
            Some(capabilities),
        ))
    }

    /// 用指定 [`Transport`] 和显式上下文窗口创建服务（测试与定制注入点）。
    pub fn with_transport(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ChatProtocolAdapter,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, OpenAiChatCompletionsServiceError> {
        let base_url = validate_base_url(base_url.into())
            .map_err(OpenAiChatCompletionsServiceError::InvalidBaseUrl)?;
        Ok(Self::build(
            base_url,
            credential,
            model,
            context_window_tokens,
            adapter,
            transport,
            None,
        ))
    }

    /// 使用已验证的 base URL 组装服务。
    fn build(
        base_url: String,
        credential: BearerCredential,
        model: impl Into<String>,
        context_window_tokens: u64,
        adapter: ChatProtocolAdapter,
        transport: Arc<dyn Transport>,
        capabilities: Option<ModelCapabilities>,
    ) -> Self {
        let capabilities = capabilities.unwrap_or_else(|| ModelCapabilities {
            reasoning: adapter.supports_reasoning(),
            image_input: false,
            tool_calls: true,
            multimodal_tool_result: adapter.tool_image_projection
                != ToolImageProjection::Unsupported,
            tool_choice: if adapter.supports_tool_choice {
                ToolChoiceCapabilities {
                    auto: true,
                    none: true,
                    required: true,
                    named: true,
                }
            } else {
                ToolChoiceCapabilities::auto_only()
            },
            streaming: true,
        });
        Self {
            base_url,
            credential,
            model: model.into(),
            context_window_tokens,
            adapter,
            transport,
            capabilities,
        }
    }

    /// 完整的 chat completions URL。
    fn completion_url(&self) -> String {
        join_endpoint(&self.base_url, "chat/completions")
    }
}

impl ModelService for OpenAiChatCompletionsService {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            // 建立前取消：以 Err 受控结束，不发出任何请求。
            if context.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let chat_request = encode_request_with_images(
                &request,
                &context.prepared_images,
                &self.adapter,
                &self.model,
            )?;
            let body = serde_json::to_vec(&chat_request).map_err(|error| {
                ModelError::Config(format!("failed to serialize the encoded request: {error}"))
            })?;
            let transport_request = TransportRequest {
                trace: context.trace.clone(),
                method: "POST".to_owned(),
                url: self.completion_url(),
                headers: vec![
                    ("content-type".to_owned(), "application/json".to_owned()),
                    ("accept".to_owned(), "text/event-stream".to_owned()),
                    (
                        "authorization".to_owned(),
                        self.credential.authorization_header(),
                    ),
                ],
                body,
            };
            // 等待响应头期间也可被取消；取消时丢弃 execute future 即关闭请求。
            let response = tokio::select! {
                result = self.transport.execute(transport_request) => result,
                () = context.cancellation.cancelled() => return Err(ModelError::Cancelled),
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => return Err(map_transport_error(error)),
            };
            if !(200..300).contains(&response.status) {
                let status = response.status;
                let headers = response.headers;
                let sample = read_error_sample(response.body, &context.cancellation).await?;
                return Err(classify_http_error(status, &headers, &sample));
            }
            let events = event_stream(response.body, self.adapter.clone(), context.cancellation);
            Ok(Box::pin(LifecycleValidator::new(Box::pin(events))) as ModelEventStream)
        })
    }
}

/// 读取非 2xx 响应的错误正文采样（上限 [`ERROR_BODY_SAMPLE_LIMIT`] 字节）。
///
/// 读取期间被取消按建立前取消处理。
async fn read_error_sample(
    body: BodyStream,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ModelError> {
    let read = async move {
        let mut sample = Vec::new();
        let mut body = body;
        while sample.len() < ERROR_BODY_SAMPLE_LIMIT {
            match body.next().await {
                Some(Ok(chunk)) => {
                    let remaining = ERROR_BODY_SAMPLE_LIMIT - sample.len();
                    sample.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                }
                // 正文读不出内容时按无正文处理，不掩盖原始状态码。
                _ => break,
            }
        }
        sample
    };
    tokio::select! {
        sample = read => Ok(sample),
        () = cancellation.cancelled() => Err(ModelError::Cancelled),
    }
}

/// 把非 2xx 响应分类为规范错误；结构化错误正文用于细化分类与诊断消息。
///
/// 错误文本只携带 Provider 的结构化诊断消息与状态码，不含原始正文。
fn classify_http_error(status: u16, headers: &[(String, String)], sample: &[u8]) -> ModelError {
    let body = serde_json::from_slice::<ChatErrorBody>(sample).ok();
    let message = body.as_ref().map(|body| body.error.message.clone());
    let retry_after_ms = parse_retry_after_ms(headers);
    match status {
        401 | 403 => ModelError::Auth(message.unwrap_or_else(|| {
            format!("provider rejected the request as unauthorized (status {status})")
        })),
        429 => ModelError::RateLimited {
            message: message
                .unwrap_or_else(|| format!("provider rate limited the request (status {status})")),
            retry_after_ms,
        },
        408 | 425 | 500..=599 => ModelError::Unavailable {
            message: message
                .unwrap_or_else(|| format!("provider temporarily unavailable (status {status})")),
            status: Some(status),
            retry_after_ms,
        },
        _ => match body {
            Some(body) => match decode_error_body(&body) {
                // 保留 decode_error_body 的细化分类；Provider 分支补上状态码。
                ModelError::Provider {
                    message,
                    status: None,
                } => ModelError::Provider {
                    message,
                    status: Some(status),
                },
                ModelError::RateLimited { message, .. } => ModelError::RateLimited {
                    message,
                    retry_after_ms,
                },
                other => other,
            },
            None => ModelError::Provider {
                message: format!(
                    "provider returned status {status} without a structured error body"
                ),
                status: Some(status),
            },
        },
    }
}

/// 只接受 Retry-After 的非负十进制秒数形式，并安全换算为毫秒。
///
/// HTTP-date、负数、非数字和乘法溢出均返回 `None`，不猜测等待时间。
fn parse_retry_after_ms(headers: &[(String, String)]) -> Option<u64> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1_000))
}

/// 保留 Transport 原始分类，避免重试等上层策略解析展示文本。
fn map_transport_error(error: TransportError) -> ModelError {
    let (kind, message) = match error {
        TransportError::Connect(message) => (ModelTransportErrorKind::Connection, message),
        TransportError::Timeout => (
            ModelTransportErrorKind::Timeout,
            "request timed out".to_owned(),
        ),
        TransportError::Interrupted(message) => (ModelTransportErrorKind::Interrupted, message),
    };
    ModelError::Transport { kind, message }
}

/// 把 SSE 字节流解码为规范事件流。
///
/// 所有退出路径都以唯一终态结束：正常 `finalize` 产出 `TurnFinished`；
/// 字节流中途失败产出 `TurnFailed(Transport)`；frame 解析、chunk 组装或
/// `finalize` 失败产出 `TurnFailed(Protocol)`；
/// 取消产出 `TurnFailed(Cancelled)`，之后不再产生任何事件。不派生后台任务，
/// 流被丢弃即关闭底层请求。
fn event_stream(
    body: BodyStream,
    adapter: ChatProtocolAdapter,
    cancellation: CancellationToken,
) -> impl Stream<Item = ModelEvent> + Send {
    async_stream::stream! {
        let mut body = body;
        let mut parser = SseParser::new();
        let mut assembler = ChunkAssembler::new(adapter);
        let mut saw_done = false;
        while !saw_done {
            if cancellation.is_cancelled() {
                yield ModelEvent::TurnFailed {
                    error: ModelError::Cancelled,
                };
                return;
            }
            // 等待下一段字节期间也可被取消（真实 Transport 会在此挂起）。
            let next = tokio::select! {
                item = body.next() => Some(item),
                () = cancellation.cancelled() => None,
            };
            let Some(item) = next else {
                yield ModelEvent::TurnFailed {
                    error: ModelError::Cancelled,
                };
                return;
            };
            let bytes = match item {
                Some(Ok(bytes)) => bytes,
                // 字节流中途失败是传输错误：即使已收到 finish_reason，也必须以
                // `TurnFailed(Transport)` 受控结束，不能把中断误报为成功。
                Some(Err(error)) => {
                    yield ModelEvent::TurnFailed {
                        error: map_transport_error(error),
                    };
                    return;
                }
                // 正常 EOF：停止读取，交给 finalize 判定
                // （缺 finish_reason 等由 finalize 产出 Protocol 错误）。
                None => break,
            };
            let frames = match parser.push(&bytes) {
                Ok(frames) => frames,
                Err(error) => {
                    yield ModelEvent::TurnFailed { error };
                    return;
                }
            };
            for frame in frames {
                // `data: [DONE]` 是 Provider 的流结束标记，之后的数据一律忽略。
                if frame.data.trim() == "[DONE]" {
                    saw_done = true;
                    break;
                }
                let chunk: ChatChunk = match serde_json::from_str(&frame.data) {
                    Ok(chunk) => chunk,
                    Err(chunk_error) => match serde_json::from_str::<ChatErrorBody>(&frame.data) {
                        Ok(body) => {
                            yield ModelEvent::TurnFailed {
                                error: decode_error_body(&body),
                            };
                            return;
                        }
                        Err(_) => {
                            yield ModelEvent::TurnFailed {
                                error: ModelError::Protocol(format!(
                                    "sse data frame is not a valid chat chunk: {chunk_error}"
                                )),
                            };
                            return;
                        }
                    },
                };
                let events = match assembler.push_chunk(&chunk) {
                    Ok(events) => events,
                    Err(error) => {
                        yield ModelEvent::TurnFailed { error };
                        return;
                    }
                };
                for event in events {
                    // 取消后不得再产出业务事件；逐事件检查保证及时受控结束。
                    if cancellation.is_cancelled() {
                        yield ModelEvent::TurnFailed {
                            error: ModelError::Cancelled,
                        };
                        return;
                    }
                    yield event;
                }
            }
        }
        if cancellation.is_cancelled() {
            yield ModelEvent::TurnFailed {
                error: ModelError::Cancelled,
            };
            return;
        }
        // `[DONE]` 或字节流结束：finalize 产出唯一终态。
        match assembler.finalize() {
            Ok(events) => {
                for event in events {
                    yield event;
                }
            }
            Err(error) => yield ModelEvent::TurnFailed { error },
        }
    }
}
