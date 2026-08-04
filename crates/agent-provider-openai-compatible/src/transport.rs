//! HTTP Transport 边界与基于 reqwest 的默认实现。
//!
//! Adapter 只面向 [`Transport`] trait 编程：生产环境使用 [`ReqwestTransport`]，
//! 需要记录时使用 [`ObservedTransport`] 透明包装，测试用 `agent-testkit` 的
//! `RecordedTransport` 回放 fixture，完全离线。

use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use agent_model::TraceContext;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_core::Stream;
use futures_util::StreamExt;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

/// 执行一次 Transport 请求的 Future。
///
/// `Err` 表示请求在响应头到达之前失败（连接、超时等）；响应正文读取阶段的
/// 失败由 [`BodyStream`] 的 `Err` 项表达。
pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>> + Send + 'a>>;

/// 响应正文的字节流；中途失败以 `Err` 项表达，之后不再产出字节。
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, TransportError>> + Send>>;

#[derive(Clone, Eq, PartialEq)]
/// 一次出站 HTTP 请求。
///
/// `Authorization` 等敏感 header 只允许由 Adapter 注入；`Debug` 输出对敏感
/// header 值与请求正文脱敏。
pub struct TransportRequest {
    /// 仅供进程内观察与并发关联的控制面信息；Transport 不得把它编码进 HTTP。
    pub trace: Option<TraceContext>,
    /// HTTP 方法，例如 `POST`。
    pub method: String,
    /// 完整请求 URL。
    pub url: String,
    /// 请求头（保持插入顺序）。
    pub headers: Vec<(String, String)>,
    /// 请求体字节。
    pub body: Vec<u8>,
}

/// 请求观察与 `Debug` 输出必须排除的敏感请求头（小写）。
const SENSITIVE_REQUEST_HEADERS: [&str; 5] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "cookie",
    "set-cookie",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 已永久排除 credential header 的出站请求快照。
///
/// 该类型只用于观察和回放，不替代真实 [`TransportRequest`]。完整请求正文仍可能
/// 包含用户高敏数据，因此宿主必须按高敏 Trace 管理。
pub struct RecordedWireRequest {
    /// HTTP 方法。
    pub method: String,
    /// 已通过 Service 构造校验、不含 credential 的完整 URL。
    pub url: String,
    /// 删除敏感项后的请求头，保持其余项的原始顺序与大小写。
    pub headers: Vec<(String, String)>,
    /// 编码后的原始请求正文；JSON 序列化时使用 Base64 字符串。
    #[serde(with = "base64_bytes")]
    pub body: Vec<u8>,
}

impl RecordedWireRequest {
    /// 从真实请求构造安全快照；敏感 header 在此处永久删除，不写占位值。
    pub fn from_transport_request(request: &TransportRequest) -> Self {
        Self {
            method: request.method.clone(),
            url: request.url.clone(),
            headers: request
                .headers
                .iter()
                .filter(|(name, _)| !is_sensitive_request_header(name))
                .cloned()
                .collect(),
            body: request.body.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// OpenAI-compatible Adapter 在真实 Transport 边界观察到的原生事实。
///
/// 事件携带请求自身的可选 [`TraceContext`]，并发调用不依赖全局“当前请求”状态。
/// 观察只覆盖 Adapter 实际消费到的流内容；消费者提前停止时不会后台 drain。
pub enum ProviderWireEvent {
    /// 即将转发给下层 Transport 的请求安全快照。
    Request {
        /// 当前逻辑调用与 attempt。
        trace: Option<TraceContext>,
        /// 已排除 credential header 的请求。
        request: RecordedWireRequest,
    },
    /// 下层 Transport 已返回响应头。
    ResponseStarted {
        /// 当前逻辑调用与 attempt。
        trace: Option<TraceContext>,
        /// HTTP 状态码。
        status: u16,
        /// 只保留允许记录项的响应头。
        headers: Vec<(String, String)>,
    },
    /// Adapter 实际消费到的一段原始响应字节。
    ResponseChunk {
        /// 当前逻辑调用与 attempt。
        trace: Option<TraceContext>,
        /// 保持原始 chunk 边界的字节；JSON 序列化时使用 Base64 字符串。
        #[serde(with = "base64_bytes")]
        bytes: Vec<u8>,
    },
    /// 请求建立失败或响应正文读取失败。
    ResponseFailed {
        /// 当前逻辑调用与 attempt。
        trace: Option<TraceContext>,
        /// 原始 Transport 错误分类与脱敏诊断。
        error: TransportError,
    },
    /// 响应正文被消费到自然 EOF。
    ResponseFinished {
        /// 当前逻辑调用与 attempt。
        trace: Option<TraceContext>,
    },
}

/// Provider wire 事件的同步观察接口。
///
/// 实现应只做快速、非阻塞的队列投递。接口不返回结果，因此观察器不能改变 Transport
/// 的成功、失败或取消语义；接收失败由宿主自行把 Trace 标记为 Incomplete。
pub trait ProviderWireObserver: Send + Sync {
    /// 接收一个已安全构造的 wire 事实。
    fn observe(&self, event: ProviderWireEvent);
}

impl fmt::Debug for TransportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                if is_sensitive_request_header(name) {
                    (name.as_str(), "<redacted>")
                } else {
                    (name.as_str(), value.as_str())
                }
            })
            .collect();
        formatter
            .debug_struct("TransportRequest")
            .field("trace", &self.trace)
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &headers)
            .field("body", &format_args!("{} bytes", self.body.len()))
            .finish()
    }
}

/// 一次入站 HTTP 响应；正文以字节流形式读取。
pub struct TransportResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应头。
    pub headers: Vec<(String, String)>,
    /// 响应正文字节流。
    pub body: BodyStream,
}

#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
/// Transport 层的失败分类。
///
/// 错误文本只包含网络层诊断（连接、超时、中断），绝不携带 `Authorization`
/// 等 header 值、请求正文或响应正文。
pub enum TransportError {
    /// 响应头到达之前失败（DNS、TLS、拒绝连接、客户端构建等）。
    #[error("request failed before the response started: {0}")]
    Connect(String),
    /// 连接或整体请求超时。
    #[error("request timed out")]
    Timeout,
    /// 响应正文读取中途失败（连接重置、对端提前关闭等）。
    #[error("response stream was interrupted: {0}")]
    Interrupted(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Transport 层的超时配置。
pub struct TransportTimeouts {
    /// 建立连接允许的最长时间。
    pub connect: Duration,
    /// 单次请求的总预算，覆盖流式响应读取完毕。
    pub request: Duration,
}

impl Default for TransportTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            request: Duration::from_secs(300),
        }
    }
}

/// 一次 HTTP 请求的可替换边界。
///
/// 实现者必须保证错误文本脱敏：不携带 header 值、请求正文和响应正文。
pub trait Transport: Send + Sync {
    /// 发出请求并等待响应头；响应正文经 [`TransportResponse::body`] 流式读取。
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a>;
}

#[derive(Clone)]
/// 为任意 [`Transport`] 增加透明 Provider wire 观察的装饰器。
///
/// 包装器不会修改转发给下层的请求、返回给上层的响应头、chunk 或错误；只向观察器
/// 发送安全副本。响应流被提前丢弃时，包装器随之停止，不派生后台读取任务。
pub struct ObservedTransport {
    inner: Arc<dyn Transport>,
    observer: Arc<dyn ProviderWireObserver>,
}

impl ObservedTransport {
    /// 包装一个下层 Transport 与同步观察器。
    pub fn new(inner: Arc<dyn Transport>, observer: Arc<dyn ProviderWireObserver>) -> Self {
        Self { inner, observer }
    }
}

impl Transport for ObservedTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        let trace = request.trace.clone();
        let inner = self.inner.clone();
        let observer = self.observer.clone();
        Box::pin(async move {
            observer.observe(ProviderWireEvent::Request {
                trace: trace.clone(),
                request: RecordedWireRequest::from_transport_request(&request),
            });
            let response = match inner.execute(request).await {
                Ok(response) => response,
                Err(error) => {
                    observer.observe(ProviderWireEvent::ResponseFailed {
                        trace,
                        error: error.clone(),
                    });
                    return Err(error);
                }
            };

            observer.observe(ProviderWireEvent::ResponseStarted {
                trace: trace.clone(),
                status: response.status,
                headers: recorded_response_headers(&response.headers),
            });

            let status = response.status;
            let headers = response.headers;
            let mut body = response.body;
            let observed_body = async_stream::stream! {
                while let Some(item) = body.next().await {
                    match &item {
                        Ok(bytes) => observer.observe(ProviderWireEvent::ResponseChunk {
                            trace: trace.clone(),
                            bytes: bytes.clone(),
                        }),
                        Err(error) => observer.observe(ProviderWireEvent::ResponseFailed {
                            trace: trace.clone(),
                            error: error.clone(),
                        }),
                    }
                    let failed = item.is_err();
                    yield item;
                    if failed {
                        return;
                    }
                }
                observer.observe(ProviderWireEvent::ResponseFinished { trace });
            };

            Ok(TransportResponse {
                status,
                headers,
                body: Box::pin(observed_body),
            })
        })
    }
}

/// 请求头名称是否属于必须永久排除的 credential 范围。
fn is_sensitive_request_header(name: &str) -> bool {
    SENSITIVE_REQUEST_HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

/// 复制允许进入 Trace 的响应头；真实响应本身不经过此筛选。
fn recorded_response_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| {
            let name = name.to_ascii_lowercase();
            matches!(
                name.as_str(),
                "content-type" | "retry-after" | "request-id" | "x-request-id"
            ) || name.starts_with("ratelimit-")
                || name.starts_with("x-ratelimit-")
        })
        .cloned()
        .collect()
}

#[derive(Debug)]
/// 基于 reqwest 的默认 [`Transport`] 实现。
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// 用默认超时配置创建（见 [`TransportTimeouts::default`]）。
    pub fn new() -> Result<Self, TransportError> {
        Self::with_timeouts(TransportTimeouts::default())
    }

    /// 用显式超时配置创建。
    pub fn with_timeouts(timeouts: TransportTimeouts) -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .build()
            .map_err(|error| {
                TransportError::Connect(format!("failed to build the http client: {error}"))
            })?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| {
                TransportError::Connect(format!("unsupported http method `{}`", request.method))
            })?;
            let mut builder = self.client.request(method, &request.url);
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            let response = builder
                .body(request.body)
                .send()
                .await
                .map_err(send_error)?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let body = response
                .bytes_stream()
                .map(|result| result.map(|chunk| chunk.to_vec()).map_err(body_error));
            Ok(TransportResponse {
                status,
                headers,
                body: Box::pin(body),
            })
        })
    }
}

/// 建立阶段的失败分类：超时单独一类，其余统一归为连接前失败。
fn send_error(error: reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Connect(error.to_string())
    }
}

/// 响应正文读取阶段的失败分类。
fn body_error(error: reqwest::Error) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout
    } else {
        TransportError::Interrupted(error.to_string())
    }
}

/// `Vec<u8>` 的字段级 Base64 serde，避免 JSON byte array 膨胀且保持任意字节无损。
mod base64_bytes {
    use super::*;

    pub(super) fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(D::Error::custom)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! 把 `agent-testkit` 的 `RecordedTransport` 适配成本 crate 的 [`Transport`]，
    //! 供 transport/stream 两组离线测试共用（trait 属于本 crate，为外部类型实现合规）。

    use agent_testkit::{BodyStep, RecordedRequest, RecordedTransport, RecordedTransportError};

    use super::{Transport, TransportError, TransportFuture, TransportRequest, TransportResponse};

    impl Transport for RecordedTransport {
        fn execute<'a>(&'a self, request: TransportRequest) -> TransportFuture<'a> {
            Box::pin(async move {
                let recorded = RecordedRequest {
                    // Trace 是进程内观察信息，不属于 HTTP fixture。
                    method: request.method,
                    url: request.url,
                    headers: request.headers,
                    body: request.body,
                };
                let response = self.exchange(recorded).await.map_err(map_recorded_error)?;
                let steps = response.body;
                let body = async_stream::stream! {
                    for step in steps {
                        match step {
                            BodyStep::Chunk(bytes) => yield Ok(bytes),
                            BodyStep::Fail(message) => {
                                yield Err(TransportError::Interrupted(message));
                                break;
                            }
                        }
                    }
                };
                Ok(TransportResponse {
                    status: response.status,
                    headers: response.headers,
                    body: Box::pin(body),
                })
            })
        }
    }

    /// 录制脚本的失败分类映射到 Transport 失败分类。
    fn map_recorded_error(error: RecordedTransportError) -> TransportError {
        match error {
            RecordedTransportError::Connect(message) => TransportError::Connect(message),
            RecordedTransportError::Interrupted { message, .. } => {
                TransportError::Interrupted(message)
            }
            RecordedTransportError::UnexpectedRequest { index, .. } => TransportError::Connect(
                format!("unexpected request #{index} beyond the fixture script"),
            ),
        }
    }
}
