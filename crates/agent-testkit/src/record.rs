//! 录制-回放 Transport：捕获出站请求，按 fixture 脚本返回 HTTP/SSE 数据。
//!
//! 这是 Adapter Transport 边界（M5 定义）的测试对应物：Adapter 的契约测试用
//! 它断言请求内容并回放录制的响应，完全不需要真实网络。

use std::{collections::VecDeque, fmt, sync::Mutex};

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一次被捕获的出站请求。
pub struct RecordedRequest {
    /// HTTP 方法，例如 `POST`。
    pub method: String,
    /// 完整请求 URL。
    pub url: String,
    /// 请求头（保持插入顺序；name 原样保存，匹配时忽略大小写）。
    pub headers: Vec<(String, String)>,
    /// 请求体字节。
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// 创建无头无体的请求。
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// 追加一个请求头。
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// 设置请求体。
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// 忽略大小写读取请求头。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 响应 body 的一个投递步骤；有序组合可以表达分块与中途断流。
pub enum BodyStep {
    /// 投递一段字节。
    Chunk(Vec<u8>),
    /// 投递到此处时连接中断。
    Fail(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一次脚本化的响应。
pub struct RecordedResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应头。
    pub headers: Vec<(String, String)>,
    /// 有序 body 投递步骤。
    pub body: Vec<BodyStep>,
}

impl RecordedResponse {
    /// 创建单块 body 的响应。
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: vec![BodyStep::Chunk(body.into())],
        }
    }

    /// 创建多分块、可注入中途断流的响应。
    pub fn chunked(status: u16, steps: Vec<BodyStep>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: steps,
        }
    }

    /// 追加一个响应头。
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// 按投递顺序收集完整 body；遇到注入的断流时返回
    /// [`RecordedTransportError::Interrupted`]，`after_bytes` 为已成功投递的字节数。
    pub fn collect_body(self) -> Result<Vec<u8>, RecordedTransportError> {
        let mut body = Vec::new();
        for step in self.body {
            match step {
                BodyStep::Chunk(chunk) => body.extend_from_slice(&chunk),
                BodyStep::Fail(message) => {
                    return Err(RecordedTransportError::Interrupted {
                        after_bytes: body.len(),
                        message,
                    });
                }
            }
        }
        Ok(body)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 录制-回放 Transport 的失败分类。
pub enum RecordedTransportError {
    /// 连接在建立前失败（DNS、TLS、拒绝连接等）。
    Connect(String),
    /// 响应 body 投递中途失败；`after_bytes` 是失败前已投递的字节数。
    Interrupted {
        /// 失败前已投递的字节数。
        after_bytes: usize,
        /// 失败描述。
        message: String,
    },
    /// 请求数超过 fixture 脚本数；提醒测试补 fixture。
    UnexpectedRequest {
        /// 这是第几个请求（从 0 计）。
        index: usize,
        /// 请求方法。
        method: String,
        /// 请求 URL。
        url: String,
    },
}

impl fmt::Display for RecordedTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordedTransportError::Connect(message) => {
                write!(formatter, "connect failed: {message}")
            }
            RecordedTransportError::Interrupted {
                after_bytes,
                message,
            } => write!(
                formatter,
                "response interrupted after {after_bytes} bytes: {message}"
            ),
            RecordedTransportError::UnexpectedRequest { index, method, url } => write!(
                formatter,
                "unexpected request #{index} ({method} {url}) beyond the fixture script"
            ),
        }
    }
}

impl std::error::Error for RecordedTransportError {}

/// 按 fixture 脚本回放的 Transport。
///
/// 每次 [`exchange`](Self::exchange) 记录请求并弹出下一条脚本响应；
/// `Err(Connect)` 脚本表示该次请求在建立前失败。
pub struct RecordedTransport {
    responses: Mutex<VecDeque<Result<RecordedResponse, RecordedTransportError>>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl RecordedTransport {
    /// 用有序响应脚本创建 Transport。
    pub fn new(
        responses: impl IntoIterator<Item = Result<RecordedResponse, RecordedTransportError>>,
    ) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 记录请求并返回下一条脚本响应。
    pub async fn exchange(
        &self,
        request: RecordedRequest,
    ) -> Result<RecordedResponse, RecordedTransportError> {
        let index = {
            let mut requests = self
                .requests
                .lock()
                .expect("recorded transport mutex poisoned");
            requests.push(request);
            requests.len() - 1
        };
        self.responses
            .lock()
            .expect("recorded transport mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                let requests = self
                    .requests
                    .lock()
                    .expect("recorded transport mutex poisoned");
                let last = &requests[index];
                Err(RecordedTransportError::UnexpectedRequest {
                    index,
                    method: last.method.clone(),
                    url: last.url.clone(),
                })
            })
    }

    /// 取出已捕获的全部请求（按到达顺序）。
    pub fn take_requests(&self) -> Vec<RecordedRequest> {
        std::mem::take(
            &mut self
                .requests
                .lock()
                .expect("recorded transport mutex poisoned"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_requests_and_replays_responses_in_order() {
        let transport = RecordedTransport::new([
            Ok(RecordedResponse::new(200, "data: first\n\n")
                .with_header("content-type", "text/event-stream")),
            Ok(RecordedResponse::new(200, "data: second\n\n")),
        ]);
        let first = transport
            .exchange(
                RecordedRequest::new("POST", "https://api.deepseek.com/chat/completions")
                    .with_header("content-type", "application/json")
                    .with_body(br#"{"model":"deepseek-reasoner"}"#.to_vec()),
            )
            .await
            .expect("first response");
        assert_eq!(first.status, 200);
        assert_eq!(
            first
                .headers
                .iter()
                .find(|(name, _)| name == "content-type"),
            Some(&("content-type".to_owned(), "text/event-stream".to_owned()))
        );
        let second = transport
            .exchange(RecordedRequest::new(
                "POST",
                "https://api.deepseek.com/chat/completions",
            ))
            .await
            .expect("second response");
        assert_eq!(
            second.collect_body().expect("full body"),
            b"data: second\n\n"
        );

        let requests = transport.take_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].header("Content-Type"), Some("application/json"));
        assert_eq!(
            requests[0].body,
            br#"{"model":"deepseek-reasoner"}"#.to_vec()
        );
        assert!(transport.take_requests().is_empty());
    }

    #[tokio::test]
    async fn replays_connect_failure_before_establishment() {
        let transport = RecordedTransport::new([Err(RecordedTransportError::Connect(
            "connection refused".to_owned(),
        ))]);
        let error = transport
            .exchange(RecordedRequest::new(
                "POST",
                "https://api.deepseek.com/chat/completions",
            ))
            .await
            .expect_err("connect must fail");
        assert_eq!(
            error,
            RecordedTransportError::Connect("connection refused".to_owned())
        );
        // 失败的请求同样被记录。
        assert_eq!(transport.take_requests().len(), 1);
    }

    #[tokio::test]
    async fn injects_mid_stream_interruption_with_byte_count() {
        let response = RecordedResponse::chunked(
            200,
            vec![
                BodyStep::Chunk(b"data: partial".to_vec()),
                BodyStep::Fail("connection reset by peer".to_owned()),
                BodyStep::Chunk(b"data: never seen".to_vec()),
            ],
        );
        let error = response.collect_body().expect_err("must be interrupted");
        assert_eq!(
            error,
            RecordedTransportError::Interrupted {
                after_bytes: b"data: partial".len(),
                message: "connection reset by peer".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn rejects_requests_beyond_the_fixture_script() {
        let transport = RecordedTransport::new([]);
        let error = transport
            .exchange(RecordedRequest::new(
                "POST",
                "https://api.deepseek.com/chat/completions",
            ))
            .await
            .expect_err("no fixture left");
        assert!(matches!(
            error,
            RecordedTransportError::UnexpectedRequest { index: 0, .. }
        ));
    }
}
