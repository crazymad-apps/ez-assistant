//! 网络准入、统一凭据认证和 Cookie 同源保护；页面来源不代表客户端身份。

use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_HEADERS,
            ACCESS_CONTROL_REQUEST_METHOD, AUTHORIZATION, COOKIE, HOST, ORIGIN, VARY,
        },
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt as _;

use super::{HttpState, error::HttpError};
use crate::access::AccessPermit;

/// Cookie 的建立与状态变更必须同源；不用于显式凭据的身份判定。
pub(super) fn require_same_origin(state: &HttpState, headers: &HeaderMap) -> Result<(), HttpError> {
    let scheme = if state.secure { "https" } else { "http" };
    let host = headers.get(HOST).and_then(|value| value.to_str().ok());
    let origin = headers.get(ORIGIN).and_then(|value| value.to_str().ok());
    if host.is_some_and(|host| origin == Some(format!("{scheme}://{host}").as_str())) {
        Ok(())
    } else {
        Err(HttpError::forbidden("Cookie 请求需要同源 Origin。"))
    }
}

// Cookies do not isolate TCP ports. Include the validated authority's port so two
// Host processes on one machine can stay logged in in the same browser.
pub(super) fn cookie_name(secure: bool, headers: &HeaderMap) -> String {
    let port = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .and_then(|authority| authority.port_u16())
        .unwrap_or(if secure { 443 } else { 80 });
    let scheme = if secure { "https" } else { "http" };
    format!("ez_host_session_{scheme}_{port}")
}

pub(super) async fn authorize_page(
    State(state): State<HttpState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Err(error) = normalize_authority(&mut request) {
        return error.into_response();
    }
    match authorize_request(&state, &request) {
        Ok(_) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

pub(super) async fn authorize(
    State(state): State<HttpState>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Err(error) = normalize_authority(&mut request) {
        return error.into_response();
    }
    let result = authorize_request(&state, &request);
    let (origin, permit) = match result {
        Ok(result) => result,
        Err(error) => {
            return with_cors(
                error.into_response(),
                request
                    .headers()
                    .get(ORIGIN)
                    .and_then(|value| value.to_str().ok()),
            );
        }
    };
    if request.method() == Method::OPTIONS {
        return preflight_response(request.headers(), origin.as_deref());
    }
    let login = request.uri().path() == "/auth/login";
    let terminal = request.uri().path() == "/user-terminals/socket";
    // WS 只允许进入有界首帧认证等待，认证通过前不能创建 PTY。
    if !login && permit.is_none() && !terminal {
        return with_cors(HttpError::unauthorized().into_response(), origin.as_deref());
    }
    if request.uri().path().ends_with("/native-path")
        && !permit.as_ref().is_some_and(|permit| permit.native)
    {
        return with_cors(
            HttpError::forbidden("该路径仅允许本机原生客户端访问。").into_response(),
            origin.as_deref(),
        );
    }
    if !terminal
        && !matches!(
            request.uri().path(),
            "/health" | "/capabilities" | "/auth/logout"
        )
    {
        let route = request
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .map_or("", |path| path.as_str());
        match super::compatibility::admit(
            request.headers(),
            request.method(),
            route,
            permit.as_ref(),
        ) {
            Ok(client) => {
                request.extensions_mut().insert(client);
            }
            Err(error) => {
                return with_cors(super::compatibility::response(error), origin.as_deref());
            }
        }
    }
    if !matches!(
        request.uri().path(),
        "/auth/login"
            | "/auth/logout"
            | "/auth/session"
            | "/health"
            | "/capabilities"
            | "/commands"
    ) && state.startup.services().is_err()
    {
        return with_cors(HttpError::unavailable().into_response(), origin.as_deref());
    }
    let streaming_response =
        request.uri().path() != "/commands" && !request.uri().path().starts_with("/auth/");
    request.extensions_mut().insert(permit.clone());
    if let Some(permit) = &permit {
        request.extensions_mut().insert(permit.clone());
        if !terminal {
            let body = std::mem::replace(request.body_mut(), Body::empty());
            *request.body_mut() = guarded_body(body, permit.clone(), false);
        }
    }
    // 不取消业务 handler：已被 Runtime 接纳的 Run 不随客户端断线撤销。
    let mut response = next.run(request).await;
    if streaming_response
        && !terminal
        && let Some(permit) = permit
    {
        let body = std::mem::replace(response.body_mut(), Body::empty());
        *response.body_mut() = guarded_body(body, permit, true);
    }
    with_cors(response, origin.as_deref())
}

/// Hyper 将 HTTP/2 的 `:authority` 放在 URI 中；统一后再校验，确保鉴权、
/// Cookie 端口隔离和页面 CSP 使用同一个目标，且拒绝相互矛盾的地址。
fn normalize_authority(request: &mut Request) -> Result<(), HttpError> {
    if request.headers().get_all(HOST).iter().count() > 1 {
        return Err(HttpError::forbidden(
            "multiple Host headers are not allowed",
        ));
    }
    if let Some(authority) = request.uri().authority() {
        let value = HeaderValue::from_str(authority.as_str())
            .map_err(|_| HttpError::forbidden("request authority is not allowed"))?;
        if let Some(host) = request.headers().get(HOST)
            && host != value
        {
            return Err(HttpError::forbidden("Host and request authority differ"));
        }
        request.headers_mut().insert(HOST, value);
    }
    Ok(())
}

pub(super) fn authorize_request(
    state: &HttpState,
    request: &Request,
) -> Result<(Option<String>, Option<AccessPermit>), HttpError> {
    let headers = request.headers();
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| HttpError::invalid_request("Host header is required"))?;
    let scheme = if state.secure { "https" } else { "http" };
    let expected_origin = format!("{scheme}://{host}");
    let url = reqwest::Url::parse(&expected_origin)
        .map_err(|_| HttpError::forbidden("Host header is not allowed"))?;
    if url.origin().ascii_serialization() != expected_origin
        || url.port_or_known_default() != Some(state.port)
    {
        return Err(HttpError::forbidden("Host header is not allowed"));
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .ok_or_else(|| HttpError::forbidden("TCP peer is required"))?
        .0;
    let server_name = url
        .host_str()
        .ok_or_else(|| HttpError::forbidden("Host header is not allowed"))?;
    let local = is_local_request(peer, server_name);
    let connections = if local {
        state.connections.clone()
    } else {
        state
            .access
            .remote_connection(server_name)
            .map_err(|_| HttpError::forbidden("非本地访问未开启或访问域名不被允许。"))?
    };
    let origin = headers
        .get(ORIGIN)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| HttpError::forbidden("Origin header is not allowed"))
        })
        .transpose()?;
    if request.method() == Method::OPTIONS {
        return Ok((origin, None));
    }
    if headers.get_all(AUTHORIZATION).iter().count() > 1 {
        return Err(HttpError::unauthorized());
    }
    let bearer = headers
        .get(AUTHORIZATION)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.strip_prefix("Bearer "))
                .ok_or_else(HttpError::unauthorized)
        })
        .transpose()?;
    // 显式凭据优先；失败绝不回退 Cookie，也不合并两种凭据的权限。
    let cookie = if bearer.is_none() {
        session_cookie(headers, &cookie_name(state.secure, headers))?
    } else {
        None
    };
    if cookie.is_some()
        && (origin.is_some()
            || !matches!(*request.method(), Method::GET | Method::HEAD)
            || request.uri().path() == "/user-terminals/socket")
    {
        require_same_origin(state, headers)?;
    }
    let token = bearer.or(cookie);
    let permit = token.and_then(|token| {
        if bearer.is_some()
            && cookie.is_none()
            && local
            && host == state.authority.as_ref()
            && token == state.access_token.as_ref()
        {
            return Some(AccessPermit::new(true, None, connections.clone()));
        }
        state
            .access
            .credentials
            .authenticate(token)
            .map(|session| AccessPermit::new(false, Some(session), connections.clone()))
    });
    // 登录页允许携带已过期 Cookie 重新输入密码；显式 Bearer 无效则拒绝。
    if bearer.is_some() && permit.is_none() {
        return Err(HttpError::unauthorized());
    }
    Ok((origin, permit))
}

fn session_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, HttpError> {
    let mut found = None;
    for header in headers.get_all(COOKIE) {
        let header = header.to_str().map_err(|_| HttpError::unauthorized())?;
        for pair in header.split(';') {
            if let Some((key, value)) = pair.trim().split_once('=')
                && key == name
            {
                if found.is_some() {
                    return Err(HttpError::unauthorized());
                }
                found = Some(value);
            }
        }
    }
    Ok(found)
}

fn guarded_body(body: Body, permit: AccessPermit, graceful_end: bool) -> Body {
    Body::from_stream(async_stream::stream! {
        let mut chunks = body.into_data_stream();
        loop {
            let next = tokio::select! {
                biased;
                () = permit.ended() => if graceful_end { None } else { Some(Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "login ended"))) },
                next = chunks.next() => next.map(|chunk| chunk.map_err(|_| std::io::Error::other("transport body failed"))),
            };
            match next {
                Some(chunk) => {
                    let failed = chunk.is_err();
                    yield chunk;
                    if failed { break; }
                },
                None => break,
            }
        }
    })
}

fn preflight_response(headers: &HeaderMap, origin: Option<&str>) -> Response {
    let Some(origin) = origin else {
        return HttpError::forbidden("CORS preflight requires an allowed Origin").into_response();
    };
    if !matches!(
        headers
            .get(ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|value| value.to_str().ok()),
        Some("GET" | "POST")
    ) {
        return HttpError::forbidden("CORS preflight method is not allowed").into_response();
    }
    if headers
        .get(ACCESS_CONTROL_REQUEST_HEADERS)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|requested| {
            requested.split(',').map(str::trim).any(|header| {
                ![
                    "authorization",
                    "content-type",
                    assistant_protocol::CLIENT_VERSION_HEADER,
                    assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
                ]
                .iter()
                .any(|allowed| header.eq_ignore_ascii_case(allowed))
            })
        })
    {
        return HttpError::forbidden("CORS preflight headers are not allowed").into_response();
    }
    with_cors(StatusCode::NO_CONTENT.into_response(), Some(origin))
}

fn with_cors(mut response: Response, origin: Option<&str>) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    if origin.is_some() {
        let headers = response.headers_mut();
        // 只开放显式凭据的跨源读取，不允许浏览器跨源携带 Cookie。
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
        headers.insert(VARY, HeaderValue::from_static("Origin"));
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET,POST,OPTIONS"),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(
                "authorization,content-type,x-ez-client-version,x-ez-min-compatible-version",
            ),
        );
    }
    response
}

fn is_local_request(peer: std::net::SocketAddr, server_name: &str) -> bool {
    peer.ip().is_loopback() && matches!(server_name, "127.0.0.1" | "localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http2_authority_preserves_cookie_port_and_rejects_conflicting_host() {
        let mut request = Request::builder()
            .version(axum::http::Version::HTTP_2)
            .uri("https://localhost:7241/auth/login")
            .body(Body::empty())
            .unwrap();
        normalize_authority(&mut request).unwrap();
        assert_eq!(request.headers()[HOST], "localhost:7241");
        assert_eq!(
            cookie_name(true, request.headers()),
            "ez_host_session_https_7241"
        );
        request
            .headers_mut()
            .insert(HOST, HeaderValue::from_static("localhost:7240"));
        assert!(normalize_authority(&mut request).is_err());
        request
            .headers_mut()
            .append(HOST, HeaderValue::from_static("localhost:7241"));
        assert!(normalize_authority(&mut request).is_err());
    }

    #[test]
    fn local_access_requires_both_real_loopback_peer_and_local_authority() {
        assert!(is_local_request(
            "127.0.0.1:1234".parse().unwrap(),
            "127.0.0.1"
        ));
        assert!(is_local_request(
            "127.0.0.1:1234".parse().unwrap(),
            "localhost"
        ));
        for host in [
            "localhost",
            "127.0.0.1",
            "127.0.0.2",
            "[::1]",
            "runtime.test",
        ] {
            assert!(!is_local_request("192.0.2.10:1234".parse().unwrap(), host));
        }
        assert!(!is_local_request(
            "127.0.0.1:1234".parse().unwrap(),
            "runtime.test"
        ));
    }
}
