//! rmcp 的 HTTP 字节边界：JSON/错误正文与 SSE 在解析前限量。
//! 仍由 rmcp 管理握手、会话、取消和工具调用，不另建协议状态机。

use std::{borrow::Cow, collections::HashMap, io, sync::Arc};

use futures_util::{StreamExt as _, TryStreamExt as _};
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue},
};
use rmcp::{
    ErrorData,
    model::{ClientJsonRpcMessage, ClientRequest, JsonRpcMessage, ServerJsonRpcMessage},
    transport::{
        common::client_side_sse::BoxedSseResponse,
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
        },
    },
};
use sse_stream::SseStream;

use super::connection::MAX_MESSAGE_BYTES;

type HttpError = StreamableHttpError<reqwest::Error>;
const SESSION_ID: &str = "mcp-session-id";

#[derive(Clone)]
pub(super) struct BoundedHttpClient(pub(super) Client);

impl StreamableHttpClient for BoundedHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            MAX_MESSAGE_BYTES,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let mut request = self
            .0
            .post(uri.as_ref())
            .header(ACCEPT, "application/json, text/event-stream");
        for (name, value) in custom_headers {
            // 与 rmcp 的保留字段一致；协议版本头由 worker 注入，必须允许通过。
            if matches!(name.as_str(), "accept" | "mcp-session-id" | "last-event-id") {
                return Err(StreamableHttpError::ReservedHeaderConflict(
                    name.to_string(),
                ));
            }
            request = request.header(name, value);
        }
        if let Some(auth) = auth_header {
            request = request.bearer_auth(auth);
        }
        if let Some(session) = &session_id {
            request = request.header(SESSION_ID, session.as_ref());
        }
        let response = request.json(&message).send().await?;
        let status = response.status();
        if matches!(status, StatusCode::ACCEPTED | StatusCode::NO_CONTENT) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == StatusCode::NOT_FOUND && session_id.is_some() {
            return Err(StreamableHttpError::SessionExpired);
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let returned_session = response
            .headers()
            .get(SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if status.is_success() && content_type.starts_with("text/event-stream") {
            let stream = bounded_sse(response, max_sse_event_size.min(MAX_MESSAGE_BYTES));
            if matches!(&message, ClientJsonRpcMessage::Request(request)
                if matches!(&request.request, ClientRequest::DiscoverRequest(_) | ClientRequest::InitializeRequest(_)))
            {
                return handshake_response(stream, returned_session).await;
            }
            return Ok(StreamableHttpPostResponse::Sse(stream, returned_session));
        }
        // 无 Content-Length、分块响应和错误正文也必须逐块核对，不能先 response.json/text。
        let body = bounded_body(response, MAX_MESSAGE_BYTES).await?;
        if !status.is_success() {
            if content_type.starts_with("application/json")
                && let Ok(message @ JsonRpcMessage::Error(_)) =
                    serde_json::from_slice::<ServerJsonRpcMessage>(&body)
            {
                return Ok(StreamableHttpPostResponse::Json(message, returned_session));
            }
            // 保留 rmcp 对旧版 HTTP Server 的握手降级信号，但不把错误正文带入日志。
            if session_id.is_none()
                && status.is_client_error()
                && !matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
                && let ClientJsonRpcMessage::Request(request) = &message
                && matches!(request.request, ClientRequest::DiscoverRequest(_))
            {
                return Ok(StreamableHttpPostResponse::Json(
                    ServerJsonRpcMessage::error(
                        ErrorData::invalid_request("server/discover rejected by HTTP server", None),
                        Some(request.id.clone()),
                    ),
                    None,
                ));
            }
            return Err(StreamableHttpError::UnexpectedServerResponse(
                Cow::Borrowed("MCP HTTP request rejected"),
            ));
        }
        if body.is_empty() && !matches!(message, ClientJsonRpcMessage::Request(_)) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if !content_type.starts_with("application/json") {
            return Err(StreamableHttpError::UnexpectedContentType(None));
        }
        Ok(StreamableHttpPostResponse::Json(
            serde_json::from_slice(&body)?,
            returned_session,
        ))
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, HttpError> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
            MAX_MESSAGE_BYTES,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxedSseResponse, HttpError> {
        // GET 的 rmcp 实现已在原始字节流限量；错误状态不读取正文。
        self.0
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size.min(MAX_MESSAGE_BYTES),
            )
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        self.0
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }
}

/// rmcp 3.2.0 的 SSE 握手读取只接受 Response，会丢弃 Error 并提前关闭 transport。
/// 仅将握手的终结响应归一为它已支持的 Json 分支；保留原始 id/code，由 rmcp 校验关联、
/// 判断 Auto 降级或拒绝。普通调用保持 SSE；不重试请求，不记录正文，也不另建握手状态。
async fn handshake_response(
    mut stream: BoxedSseResponse,
    session_id: Option<String>,
) -> Result<StreamableHttpPostResponse, HttpError> {
    while let Some(event) = stream.try_next().await? {
        let data = event.data.unwrap_or_default();
        if data.trim().is_empty() {
            continue;
        }
        let message = serde_json::from_str::<ServerJsonRpcMessage>(&data)?;
        if matches!(
            message,
            JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_)
        ) {
            return Ok(StreamableHttpPostResponse::Json(message, session_id));
        }
    }
    Err(StreamableHttpError::UnexpectedServerResponse(
        Cow::Borrowed("MCP handshake SSE stream ended without a response"),
    ))
}

async fn bounded_body(mut response: Response, limit: usize) -> Result<Vec<u8>, HttpError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(too_large().into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(too_large().into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn bounded_sse(response: Response, limit: usize) -> BoxedSseResponse {
    let stream = async_stream::try_stream! {
        let mut source = response.bytes_stream();
        let mut event_bytes = 0usize;
        let mut line_bytes = 0usize;
        let mut previous_cr = false;
        while let Some(chunk) = source.try_next().await.map_err(io::Error::other)? {
            for byte in &chunk {
                event_bytes = event_bytes.saturating_add(1);
                if event_bytes > limit { Err(too_large())?; }
                if *byte == b'\n' && previous_cr {
                    previous_cr = false;
                    continue;
                }
                previous_cr = *byte == b'\r';
                if matches!(*byte, b'\n' | b'\r') {
                    if line_bytes == 0 { event_bytes = 0; }
                    line_bytes = 0;
                } else {
                    line_bytes += 1;
                }
            }
            yield chunk;
        }
    };
    SseStream::from_bytes_stream(stream.map_err(|error: io::Error| error)).boxed()
}

fn too_large() -> io::Error {
    io::Error::other("MCP HTTP message exceeds the size limit")
}

#[cfg(test)]
mod tests;
