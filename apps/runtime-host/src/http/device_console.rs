//! 受控终端控制台 HTTP 反向代理：固定对端 IP、禁用重定向、限制正文并屏蔽敏感配置路径。

use std::{sync::OnceLock, time::Duration};

use assistant_protocol::DeviceId;
use axum::{
    Extension,
    body::Bytes,
    extract::{Path, RawQuery},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, ETAG, LAST_MODIFIED},
    },
    response::{IntoResponse, Response},
};
use reqwest::redirect::Policy;

use super::{ReadyServices, error::HttpError};
use crate::{
    access::AccessPermit,
    device::{ConsoleProxyPathError, DeviceConsoleTarget, resolve_proxy_url},
};

pub(super) const MAX_DEVICE_CONSOLE_BODY_BYTES: usize = 10 * 1024 * 1024;
const CONSOLE_PROXY_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn proxy(
    Extension(services): Extension<std::sync::Arc<ReadyServices>>,
    Extension(permit): Extension<AccessPermit>,
    Path((device_id, relative_path)): Path<(String, String)>,
    RawQuery(raw_query): RawQuery,
    method: Method,
    req_headers: HeaderMap,
    body: Bytes,
) -> Response {
    if permit.check().is_err() {
        return HttpError::unauthorized().into_response();
    }
    if !permit.native {
        return HttpError::forbidden("只能通过本机客户端访问终端控制面板。").into_response();
    }
    let Ok(device_id) = DeviceId::new(device_id) else {
        return HttpError::invalid_request("设备 ID 无效。").into_response();
    };
    if let Some(target) = services.device_gateway.console_target(&device_id).await {
        return proxy_to_target(
            &target,
            &relative_path,
            raw_query.as_deref(),
            method,
            &req_headers,
            body,
        )
        .await;
    }
    if services.device_gateway.has_native_console(&device_id).await {
        return services
            .device_gateway
            .handle_native_console(&device_id, &relative_path, method, body, &services.speech)
            .await;
    }
    (StatusCode::NOT_FOUND, "device console unavailable").into_response()
}

async fn proxy_to_target(
    target: &DeviceConsoleTarget,
    relative_path: &str,
    raw_query: Option<&str>,
    method: Method,
    req_headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let reqwest_method = match method {
        Method::GET => reqwest::Method::GET,
        Method::POST => reqwest::Method::POST,
        Method::PUT => reqwest::Method::PUT,
        Method::DELETE => reqwest::Method::DELETE,
        _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
    };
    if body.len() > MAX_DEVICE_CONSOLE_BODY_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }

    let upstream_url = match resolve_proxy_url(target, relative_path, raw_query) {
        Ok(url) => url,
        Err(ConsoleProxyPathError::InvalidPath) => {
            return HttpError::invalid_request("终端控制台路径格式无效。").into_response();
        }
        Err(ConsoleProxyPathError::ForbiddenPath) => {
            return HttpError::forbidden("该终端控制台路径受策略限制，禁止访问。").into_response();
        }
    };

    let mut upstream_req = shared_client().request(reqwest_method, upstream_url);
    for header in [CONTENT_TYPE, ACCEPT] {
        if let Some(value) = req_headers.get(&header)
            && let Ok(text) = value.to_str()
        {
            upstream_req = upstream_req.header(header.as_str(), text);
        }
    }
    if !body.is_empty() {
        upstream_req = upstream_req.body(body);
    }

    let mut upstream_res = match upstream_req.send().await {
        Ok(res) => res,
        Err(_) => {
            return (StatusCode::BAD_GATEWAY, "device console request failed").into_response();
        }
    };
    if upstream_res.status().is_redirection() {
        return (
            StatusCode::BAD_GATEWAY,
            "device console upstream redirects are not allowed",
        )
            .into_response();
    }

    let status =
        StatusCode::from_u16(upstream_res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response_headers = HeaderMap::new();
    for header in [CONTENT_TYPE, CACHE_CONTROL, ETAG, LAST_MODIFIED] {
        if let Some(value) = upstream_res.headers().get(header.as_str())
            && let Ok(parsed) = HeaderValue::from_bytes(value.as_bytes())
        {
            response_headers.insert(header, parsed);
        }
    }

    if upstream_res
        .content_length()
        .is_some_and(|length| length > MAX_DEVICE_CONSOLE_BODY_BYTES as u64)
    {
        return (
            StatusCode::BAD_GATEWAY,
            "device console response exceeded limit",
        )
            .into_response();
    }

    let mut payload = Vec::new();
    loop {
        match upstream_res.chunk().await {
            Ok(Some(chunk)) => {
                if payload.len().saturating_add(chunk.len()) > MAX_DEVICE_CONSOLE_BODY_BYTES {
                    return (
                        StatusCode::BAD_GATEWAY,
                        "device console response exceeded limit",
                    )
                        .into_response();
                }
                payload.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => {
                return (StatusCode::BAD_GATEWAY, "device console read failed").into_response();
            }
        }
    }

    (status, response_headers, payload).into_response()
}

fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(CONSOLE_PROXY_TIMEOUT)
            .build()
            .expect("build device console proxy client")
    })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use axum::{
        Json, Router,
        body::to_bytes,
        http::header::{AUTHORIZATION, COOKIE, LOCATION, SET_COOKIE},
        routing::{get, post},
    };
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;
    use crate::device::DeviceConsoleTarget;

    fn loopback_target(port: u16) -> DeviceConsoleTarget {
        DeviceConsoleTarget {
            scheme: crate::device::DeviceMcpScheme::Http,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            api_prefix: "/api/v1".to_owned(),
            ui_path: Some("/ui/".to_owned()),
        }
    }

    #[tokio::test]
    async fn proxy_forwards_allowed_routes_strips_sensitive_headers_and_blocks_redirects_and_deepseek()
     {
        let app = Router::new()
            .route(
                "/api/v1/devices",
                get(|headers: HeaderMap| async move {
                    assert!(headers.get(AUTHORIZATION).is_none());
                    assert!(headers.get(COOKIE).is_none());
                    let mut out_headers = HeaderMap::new();
                    out_headers.insert(SET_COOKIE, HeaderValue::from_static("sid=secret"));
                    (out_headers, Json(json!({ "devices": [{ "id": "esp-1" }] })))
                }),
            )
            .route(
                "/api/v1/voice/transcribe",
                post(
                    |body: Bytes| async move { Json(json!({ "len": body.len(), "text": "你好" })) },
                ),
            )
            .route(
                "/api/v1/redirect",
                get(|| async {
                    let mut out_headers = HeaderMap::new();
                    out_headers.insert(LOCATION, HeaderValue::from_static("http://example.com"));
                    (StatusCode::FOUND, out_headers, "redirect")
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock");
        });
        let target = loopback_target(port);

        let mut req_headers = HeaderMap::new();
        req_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer top-secret"));
        req_headers.insert(COOKIE, HeaderValue::from_static("session=abc"));
        req_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let res = proxy_to_target(
            &target,
            "api/v1/devices",
            None,
            Method::GET,
            &req_headers,
            Bytes::new(),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(SET_COOKIE).is_none());
        let bytes = to_bytes(res.into_body(), 1024 * 1024).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(json["devices"][0]["id"], "esp-1");

        let post_res = proxy_to_target(
            &target,
            "api/v1/voice/transcribe",
            Some("lang=zh"),
            Method::POST,
            &HeaderMap::new(),
            Bytes::from_static(b"{\"audio_base64\":\"AA==\"}"),
        )
        .await;
        assert_eq!(post_res.status(), StatusCode::OK);
        let post_bytes = to_bytes(post_res.into_body(), 1024 * 1024)
            .await
            .expect("post body");
        let post_json: serde_json::Value = serde_json::from_slice(&post_bytes).expect("post json");
        assert_eq!(post_json["text"], "你好");

        let deepseek_res = proxy_to_target(
            &target,
            "api/v1/settings/deepseek",
            None,
            Method::GET,
            &HeaderMap::new(),
            Bytes::new(),
        )
        .await;
        assert_eq!(deepseek_res.status(), StatusCode::FORBIDDEN);

        let traversal_res = proxy_to_target(
            &target,
            "api/v1/../healthz",
            None,
            Method::GET,
            &HeaderMap::new(),
            Bytes::new(),
        )
        .await;
        assert_eq!(traversal_res.status(), StatusCode::BAD_REQUEST);

        let redirect_res = proxy_to_target(
            &target,
            "api/v1/redirect",
            None,
            Method::GET,
            &HeaderMap::new(),
            Bytes::new(),
        )
        .await;
        assert_eq!(redirect_res.status(), StatusCode::BAD_GATEWAY);

        server.abort();
    }
}
