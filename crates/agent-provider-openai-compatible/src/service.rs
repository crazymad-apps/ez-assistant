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

use std::{fmt, sync::Arc};

use agent_model::{
    LifecycleValidator, ModelCallContext, ModelCapabilities, ModelError, ModelEvent,
    ModelEventStream, ModelRequest, ModelService, ModelStreamFuture,
};
use futures_core::Stream;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    BodyStream, ChatChunk, ChatErrorBody, ChunkAssembler, Profile, ReqwestTransport, SseParser,
    Transport, TransportError, TransportRequest, TransportTimeouts, decode_error_body,
    encode_request,
};

/// 非 2xx 响应错误正文的采样上限（字节）；错误分类不读取完整正文。
const ERROR_BODY_SAMPLE_LIMIT: usize = 2048;

#[derive(Clone)]
/// OpenAI-compatible Bearer credential。
///
/// 只允许由 Adapter 写入 `Authorization` header；`Debug` 输出脱敏，
/// credential 不进入请求 DTO、事件和任何错误文本。
pub struct BearerCredential(String);

impl BearerCredential {
    /// 用 bearer token 创建凭据。
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// 写入 `Authorization` header 的值。
    fn authorization_header(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for BearerCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerCredential(<redacted>)")
    }
}

/// OpenAI Chat Completions compatible 的单次模型 Turn 服务。
///
/// 构造时注入 base URL、credential、模型名、[`Profile`] 与 [`Transport`]——一个
/// 服务实例就是一条编译完成的模型配置，调用目标在此固定，请求不再携带路由信息。
/// credential 只在发请求时写入 `Authorization` header，不进入 Debug、事件和错误文本。
pub struct OpenAiCompatibleService {
    /// Provider API 的 base URL；请求发往 `{base_url}/chat/completions`。
    base_url: String,
    /// 注入的 Bearer credential。
    credential: BearerCredential,
    /// Provider 侧的模型名称，例如 `deepseek-v4-flash`。
    model: String,
    /// Provider 方言配置。
    profile: Profile,
    /// 底层 HTTP Transport。
    transport: Arc<dyn Transport>,
    /// 由 Profile 推导的能力声明。
    capabilities: ModelCapabilities,
}

impl OpenAiCompatibleService {
    /// 用默认 [`ReqwestTransport`] 创建服务。
    pub fn new(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        profile: Profile,
        timeouts: TransportTimeouts,
    ) -> Result<Self, TransportError> {
        let transport = ReqwestTransport::with_timeouts(timeouts)?;
        Ok(Self::with_transport(
            base_url,
            credential,
            model,
            profile,
            Arc::new(transport),
        ))
    }

    /// 用指定 [`Transport`] 创建服务（测试与定制注入点）。
    pub fn with_transport(
        base_url: impl Into<String>,
        credential: BearerCredential,
        model: impl Into<String>,
        profile: Profile,
        transport: Arc<dyn Transport>,
    ) -> Self {
        let capabilities = ModelCapabilities {
            reasoning: profile.reasoning_content_field.is_some(),
            tool_calls: true,
            streaming: true,
        };
        Self {
            base_url: base_url.into(),
            credential,
            model: model.into(),
            profile,
            transport,
            capabilities,
        }
    }

    /// 完整的 chat completions URL。
    fn completion_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

impl ModelService for OpenAiCompatibleService {
    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            // 建立前取消：以 Err 受控结束，不发出任何请求。
            if context.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let chat_request = encode_request(&request, &self.profile, &self.model)?;
            let body = serde_json::to_vec(&chat_request).map_err(|error| {
                ModelError::Config(format!("failed to serialize the encoded request: {error}"))
            })?;
            let transport_request = TransportRequest {
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
                Err(error) => return Err(ModelError::Transport(error.to_string())),
            };
            if !(200..300).contains(&response.status) {
                let sample = read_error_sample(response.body, &context.cancellation).await?;
                return Err(classify_http_error(response.status, &sample));
            }
            let events = event_stream(response.body, self.profile.clone(), context.cancellation);
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
fn classify_http_error(status: u16, sample: &[u8]) -> ModelError {
    let body = serde_json::from_slice::<ChatErrorBody>(sample).ok();
    let message = body.as_ref().map(|body| body.error.message.clone());
    match status {
        401 | 403 => ModelError::Auth(message.unwrap_or_else(|| {
            format!("provider rejected the request as unauthorized (status {status})")
        })),
        429 => ModelError::RateLimited(
            message
                .unwrap_or_else(|| format!("provider rate limited the request (status {status})")),
        ),
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

/// 把 SSE 字节流解码为规范事件流。
///
/// 所有退出路径都以唯一终态结束：正常 `finalize` 产出 `TurnFinished`；
/// 字节流中途失败产出 `TurnFailed(Transport)`；frame 解析、chunk 组装或
/// `finalize` 失败产出 `TurnFailed(Protocol)`；
/// 取消产出 `TurnFailed(Cancelled)`，之后不再产生任何事件。不派生后台任务，
/// 流被丢弃即关闭底层请求。
fn event_stream(
    body: BodyStream,
    profile: Profile,
    cancellation: CancellationToken,
) -> impl Stream<Item = ModelEvent> + Send {
    async_stream::stream! {
        let mut body = body;
        let mut parser = SseParser::new();
        let mut assembler = ChunkAssembler::new(profile);
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
                        error: ModelError::Transport(error.to_string()),
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
                    Err(error) => {
                        yield ModelEvent::TurnFailed {
                            error: ModelError::Protocol(format!(
                                "sse data frame is not a valid chat chunk: {error}"
                            )),
                        };
                        return;
                    }
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
