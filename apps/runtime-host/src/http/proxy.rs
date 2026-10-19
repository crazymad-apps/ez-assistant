//! 原 HTTP 入口的目标分流；目标复用业务 handler，本层只拥有传输状态。
mod allowed;
mod paths;
#[cfg(test)]
mod tests;
mod transfer;
use super::{
    HttpState, MAX_COMMAND_BYTES, auth,
    commands::{self, CommandRequest},
    compatibility,
};
use crate::access::{
    AccessPermit,
    registration::network::streams::{Flow, Frame, StreamError},
};
use assistant_protocol::{ClientCompatibility, check_compatibility};
use axum::{
    Extension, Json,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;
use tower::ServiceExt;

const DEVICE_HEADER: &str = "x-ez-target-device";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestHead {
    method: String,
    path: String,
    client: ClientCompatibility,
    headers: Vec<(String, String)>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseHead {
    status: u16,
    headers: Vec<(String, String)>,
}

fn target(request: &Request) -> Result<Option<String>, ()> {
    let headers: Vec<_> = request.headers().get_all(DEVICE_HEADER).iter().collect();
    let queries: Vec<_> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .filter(|(key, _)| key == "target_device")
            .map(|(_, value)| value.into_owned())
            .collect();
    if headers.len() > 1 || queries.len() > 1 {
        return Err(());
    }
    let header = headers
        .first()
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| ())?;
    if header
        .as_ref()
        .zip(queries.first())
        .is_some_and(|(a, b)| a != b)
    {
        return Err(());
    }
    let value = header.or_else(|| queries.first().cloned());
    if value
        .as_ref()
        .is_some_and(|v| !crate::access::registration::valid_device_id(v))
    {
        return Err(());
    }
    Ok(value)
}

pub(super) async fn route(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    let device = match target(&request) {
        Ok(None) => return next.run(request).await,
        Ok(Some(device)) => device,
        Err(()) => return failure(None, "network_target_invalid", false),
    };
    // WS 的凭据和版本在首帧验证；保留原 Upgrade 来源检查，不能提前打开本机用户域。
    if request.method() == axum::http::Method::GET
        && request.uri().path() == "/user-terminals/socket"
    {
        if url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .any(|(name, _)| name != "target_device")
        {
            return failure(Some(&device), "network_target_invalid", false);
        }
        let mut request = request;
        request
            .extensions_mut()
            .insert(super::terminals::Target(device));
        return next.run(request).await;
    }
    let Some(mut permit) = request
        .extensions()
        .get::<Option<AccessPermit>>()
        .cloned()
        .flatten()
    else {
        return super::error::HttpError::unauthorized().into_response();
    };
    if permit.check().is_err() || permit.native || permit.user_key().is_none() {
        return failure(Some(&device), "network_access_denied", false);
    }
    if !request
        .headers()
        .contains_key(axum::http::header::AUTHORIZATION)
        && auth::expected_login_context(&request).as_deref() != permit.login_context().as_deref()
    {
        return (StatusCode::CONFLICT, Json(serde_json::json!({"error":{"code":"login_context_changed","message":"当前登录账号已变化，请刷新页面。"}}))).into_response();
    }
    let Some(kind) = paths::route(request.method().as_str(), request.uri().path()) else {
        return failure(Some(&device), "network_operation_denied", false);
    };
    if url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .any(|(name, _)| name != "target_device" && name != "login_context")
    {
        return failure(Some(&device), "network_target_invalid", false);
    }
    let client = match compatibility::declaration(request.headers()) {
        Ok(declared) => match declared.or_else(|| {
            (kind == paths::Route::Media)
                .then(|| permit.compatibility().cloned())
                .flatten()
        }) {
            Some(client) => client,
            None => return failure(Some(&device), "network_incompatible", false),
        },
        Err(error) => return compatibility::response(error),
    };
    if let Err(error) = check_compatibility(Some(&client), &ClientCompatibility::current()) {
        return compatibility::response(error);
    }
    if state
        .access
        .credentials
        .verify_enterprise(&mut permit)
        .await
        .is_err()
    {
        return failure(Some(&device), "network_access_denied", false);
    }
    let head = RequestHead {
        method: request.method().to_string(),
        path: request.uri().path().to_string(),
        client,
        headers: match paths::headers(request.headers()) {
            Ok(headers) => headers,
            Err(()) => return failure(Some(&device), "network_target_invalid", false),
        },
    };
    let body = if kind == paths::Route::Command {
        let bytes = tokio::select! {
            () = permit.ended() => return failure(Some(&device), "network_access_denied", false),
            result = timeout(Duration::from_secs(60), to_bytes(request.into_body(), MAX_COMMAND_BYTES)) => match result { Ok(Ok(body)) => body, _ => return failure(Some(&device), "network_target_invalid", false) },
        };
        let command = match serde_json::from_slice::<CommandRequest>(&bytes) {
            Ok(command) => command,
            Err(_) => return failure(Some(&device), "network_target_invalid", false),
        };
        if !allowed::allowed(&command) {
            return failure(Some(&device), "network_operation_denied", false);
        }
        Body::from(bytes)
    } else {
        request.into_body()
    };
    let flow = match state
        .access
        .network
        .open_flow(
            device.clone(),
            permit.clone(),
            crate::access::registration::network::streams::Kind::Http,
        )
        .await
    {
        Ok(flow) => flow,
        Err(crate::access::AccessError::Busy) => {
            return failure(Some(&device), "network_busy", false);
        }
        Err(
            crate::access::AccessError::NetworkDenied | crate::access::AccessError::Unauthorized,
        ) => return failure(Some(&device), "network_access_denied", false),
        Err(_) => return failure(Some(&device), "network_target_unavailable", false),
    };
    if let Err(error) = flow.ready().await {
        return stream_failure(&device, error, false);
    }
    // 从发送请求头开始保守地标记未知；任何阶段均不重放写意图。
    let flow = Arc::new(flow);
    if let Err(error) = flow
        .send(Frame {
            kind: 1,
            payload: serde_json::to_vec(&head).expect("serializable HTTP head"),
        })
        .await
    {
        return stream_failure(&device, error, true);
    }
    let mut upload = transfer::upload(flow.clone(), body, head_length(&head.headers));
    let result = transfer::response_head(&flow, &mut upload).await;
    let head = match result {
        Ok(head) => head,
        Err(error) => return stream_failure(&device, error, true),
    };
    // 空正文不会被 HTTP 层轮询，必须在发送成功响应头前确认 FIN。
    let length = head_length(&head.headers);
    let body = if length == Some(0) || head.status == 204 || head.status == 304 {
        match timeout(
            Duration::from_secs(30),
            transfer::receive(&flow, &mut upload),
        )
        .await
        {
            Ok(Ok(frame)) if frame.kind == 4 => Body::empty(),
            _ => return failure(Some(&device), "network_stream_failed", true),
        }
    } else {
        transfer::incoming(flow, length, upload, kind != paths::Route::Events)
    };
    let mut response = Response::new(body);
    *response.status_mut() = match StatusCode::from_u16(head.status) {
        Ok(status)
            if (status == StatusCode::NOT_MODIFIED || !status.is_redirection())
                && !status.is_informational() =>
        {
            status
        }
        _ => return failure(Some(&device), "network_stream_failed", true),
    };
    for (name, value) in head.headers {
        if response_header(&name) {
            let Ok(name) = axum::http::HeaderName::try_from(name) else {
                return failure(Some(&device), "network_stream_failed", true);
            };
            let Ok(value) = axum::http::HeaderValue::try_from(value) else {
                return failure(Some(&device), "network_stream_failed", true);
            };
            response.headers_mut().insert(name, value);
        }
    }
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}
fn head_length(headers: &[(String, String)]) -> Option<u64> {
    headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
}
fn response_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-length"
            | "content-disposition"
            | "content-range"
            | "accept-ranges"
            | "etag"
            | "last-modified"
            | "retry-after"
            | "x-content-type-options"
    )
}
fn stream_failure(device: &str, error: StreamError, forwarded: bool) -> Response {
    failure(
        Some(device),
        match error {
            StreamError::Busy => "network_busy",
            StreamError::Timeout => "network_timeout",
            _ => "network_stream_failed",
        },
        forwarded,
    )
}
fn failure(device: Option<&str>, code: &str, forwarded: bool) -> Response {
    let status = match code {
        "network_target_invalid" => StatusCode::BAD_REQUEST,
        "network_access_denied" | "network_operation_denied" => StatusCode::FORBIDDEN,
        "network_busy" => StatusCode::TOO_MANY_REQUESTS,
        "network_incompatible" => StatusCode::CONFLICT,
        "network_target_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        "network_timeout" => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, Json(serde_json::json!({"error":{"code":code,"message":"设备请求未能完成。","target_device":device,"outcome":if forwarded { "unknown" } else { "not_executed" }}}))).into_response()
}

/// 工作 future 由组网连接 owner 轮询；取消仅结束传输，不调用 Runtime 停止或全局退出。
pub(crate) async fn serve(state: HttpState, flow: Flow, permit: AccessPermit) {
    let flow = Arc::new(flow);
    let ended = flow.ended();
    let reset = flow.cancelled();
    tokio::select! {
        biased;
        () = ended.cancelled() => {},
        () = reset.cancelled() => {},
        () = permit.ended() => {},
        _ = serve_inner(state, flow.clone(), permit.clone()) => {},
    }
}
async fn serve_inner(
    state: HttpState,
    flow: Arc<Flow>,
    permit: AccessPermit,
) -> Result<(), StreamError> {
    let frame = timeout(Duration::from_secs(60), flow.recv())
        .await
        .map_err(|_| StreamError::Timeout)??;
    let head: RequestHead =
        serde_json::from_slice(&frame.payload).map_err(|_| StreamError::Invalid)?;
    if frame.kind != 1 {
        return Err(StreamError::Invalid);
    }
    let kind = paths::route(&head.method, &head.path).ok_or(StreamError::Invalid)?;
    let headers = paths::restore(&head.headers).map_err(|_| StreamError::Invalid)?;
    if let Err(error) = check_compatibility(Some(&head.client), &ClientCompatibility::current()) {
        return send_response(&flow, compatibility::response(error)).await;
    }
    permit.check().map_err(|_| StreamError::Ended)?;
    let body = transfer::incoming(flow.clone(), head_length(&head.headers), None, true);
    if kind != paths::Route::Command {
        let mut request = Request::builder()
            .method(head.method.as_str())
            .uri(head.path)
            .body(body)
            .map_err(|_| StreamError::Invalid)?;
        *request.headers_mut() = headers;
        request.extensions_mut().insert(Some(permit.clone()));
        request.extensions_mut().insert(permit);
        request.extensions_mut().insert(head.client);
        let response = super::routes::network_resources(state)
            .oneshot(request)
            .await
            .map_err(|never| match never {})?;
        return send_response(&flow, response).await;
    }
    let body = to_bytes(body, MAX_COMMAND_BYTES)
        .await
        .map_err(|_| StreamError::Invalid)?;
    let request: CommandRequest =
        serde_json::from_slice(&body).map_err(|_| StreamError::Invalid)?;
    if !allowed::allowed(&request) {
        return send_response(&flow, failure(None, "network_operation_denied", false)).await;
    }
    permit.check().map_err(|_| StreamError::Ended)?;
    let response =
        commands::handle_command(State(state), Extension(permit), Ok(Json(request))).await;
    send_response(&flow, response).await
}
async fn send_response(flow: &Flow, response: Response) -> Result<(), StreamError> {
    if response.headers().contains_key("content-encoding") {
        return Err(StreamError::Invalid);
    }
    let head = ResponseHead {
        status: response.status().as_u16(),
        headers: filtered_headers(response.headers()),
    };
    flow.send(Frame {
        kind: 2,
        payload: serde_json::to_vec(&head).map_err(|_| StreamError::Invalid)?,
    })
    .await?;
    let mut body = response.into_body().into_data_stream();
    while let Some(bytes) = body.next().await {
        flow.bytes(&bytes.map_err(|_| StreamError::Ended)?).await?;
    }
    flow.send(Frame {
        kind: 4,
        payload: vec![],
    })
    .await?;
    flow.finish().await
}
fn filtered_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| response_header(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}
