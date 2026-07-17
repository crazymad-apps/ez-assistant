//! HTTP Transport 边界与基于 reqwest 的默认实现。
//!
//! Adapter 只面向 [`Transport`] trait 编程：生产环境使用 [`ReqwestTransport`]，
//! 测试用 `agent-testkit` 的 `RecordedTransport` 回放 fixture，完全离线。

use std::{fmt, future::Future, pin::Pin, time::Duration};

use futures_core::Stream;
use futures_util::StreamExt;
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
    /// HTTP 方法，例如 `POST`。
    pub method: String,
    /// 完整请求 URL。
    pub url: String,
    /// 请求头（保持插入顺序）。
    pub headers: Vec<(String, String)>,
    /// 请求体字节。
    pub body: Vec<u8>,
}

/// `Debug` 输出时需要脱敏值的请求头（小写）。
const REDACTED_HEADERS: [&str; 4] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "cookie",
];

impl fmt::Debug for TransportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                if REDACTED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                    (name.as_str(), "<redacted>")
                } else {
                    (name.as_str(), value.as_str())
                }
            })
            .collect();
        formatter
            .debug_struct("TransportRequest")
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

#[derive(Clone, Debug, Eq, Error, PartialEq)]
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
