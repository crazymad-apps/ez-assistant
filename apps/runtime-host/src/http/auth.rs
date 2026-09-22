//! HTTP 安全边界；各路由需要的检查在 `routes` 中组合，不在这里维护路径白名单。
//!
//! `authorize` 只处理网络来源、凭据解析和 CORS。其余中间件分别约束登录、
//! 用户身份、就绪服务和传输生命周期；页面来源与本机证明都不能替代用户身份。

use axum::{
    Extension,
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

/// Cookie 本身不隔离端口；把已校验的协议和端口写入名称，避免同机多个 Host 覆盖登录。
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

/// 所有 API 的最外层：包括错误响应在内统一补充 CORS，并在业务鉴权前结束合法预检。
/// 这里允许缺少登录；是否必须登录由路由组的 `require_login` 决定。
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
    request.extensions_mut().insert(permit.clone());
    if let Some(permit) = permit {
        request.extensions_mut().insert(permit);
    }
    with_cors(next.run(request).await, origin.as_deref())
}

pub(super) async fn require_login(
    Extension(permit): Extension<Option<AccessPermit>>,
    request: Request,
    next: Next,
) -> Response {
    if permit.is_none() {
        return HttpError::unauthorized().into_response();
    }
    next.run(request).await
}

/// 浏览器 WS 握手也是 Cookie 操作，即使使用 GET、Cookie 已过期，也必须显式同源。
/// 没有 Cookie 的连接仍可进入有界首帧认证；这里不把握手成功当成已登录。
pub(super) async fn require_socket_origin(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    if !request.headers().contains_key(AUTHORIZATION) {
        match session_cookie(
            request.headers(),
            &cookie_name(state.secure, request.headers()),
        ) {
            Ok(Some(_)) => {
                if let Err(error) = require_same_origin(&state, request.headers()) {
                    return error.into_response();
                }
            }
            Ok(None) => {}
            Err(error) => return error.into_response(),
        }
    }
    next.run(request).await
}

/// 同源标签页共用 Cookie；业务请求必须证明页面仍属于当前账号，避免旧页面操作新账号。
/// Bearer 已经固定身份，无需该提示；登录与身份查询也不能要求尚未取得的上下文。
pub(super) async fn require_login_context(
    Extension(permit): Extension<AccessPermit>,
    request: Request,
    next: Next,
) -> Response {
    if !permit.native
        && !request.headers().contains_key(AUTHORIZATION)
        && expected_login_context(&request).as_deref() != permit.login_context().as_deref()
    {
        return (
            StatusCode::CONFLICT,
            axum::Json(serde_json::json!({"error": {
                "code": "login_context_changed",
                "message": "当前登录账号已变化，请刷新页面。"
            }})),
        )
            .into_response();
    }
    next.run(request).await
}

/// 个人本机凭据仍代表个人域；企业本机凭据只管理 Host，不能选择任一企业用户域。
/// WS 首帧认证另有入口，所以这里不承担“必须登录”的职责。
pub(super) async fn reject_enterprise_bootstrap(
    State(state): State<HttpState>,
    Extension(permit): Extension<Option<AccessPermit>>,
    request: Request,
    next: Next,
) -> Response {
    if state.access.center().is_some() && permit.is_some_and(|permit| permit.native) {
        return HttpError::forbidden("本机管理凭据不能访问企业用户数据。").into_response();
    }
    next.run(request).await
}

/// 复核后把更新的 permit 交给后续处理；Host 管理身份不持有 Center 登录。
/// 退出不经过此层，确保中心离线或凭据失效时仍能在本机结束任务与登录。
pub(super) async fn verify_enterprise(
    State(state): State<HttpState>,
    Extension(mut permit): Extension<AccessPermit>,
    mut request: Request,
    next: Next,
) -> Response {
    if state.access.center().is_some()
        && !permit.native
        && let Err(error) = state
            .access
            .credentials
            .verify_enterprise(&mut permit)
            .await
    {
        return super::login::access_error(error);
    }
    request.extensions_mut().insert(Some(permit.clone()));
    request.extensions_mut().insert(permit);
    next.run(request).await
}

/// 版本准入通过后才打开用户服务；同一请求的所有资源操作固定使用这一个用户域。
pub(super) async fn bind_user_services(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
    mut request: Request,
    next: Next,
) -> Response {
    match state.user_services(&permit).await {
        Ok(services) => {
            request.extensions_mut().insert(services);
        }
        Err(error) => return error.into_response(),
    }
    next.run(request).await
}

pub(super) async fn require_local_proof(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
    request: Request,
    next: Next,
) -> Response {
    if !permit.native && !has_local_proof(&state, &request) {
        return HttpError::forbidden("该路径仅允许本机原生客户端访问。").into_response();
    }
    next.run(request).await
}

/// 取消正文读取，使未完成上传进入既有清理路径；不丢弃 handler，已接纳的 Run 继续执行。
pub(super) async fn guard_request_body(
    Extension(permit): Extension<Option<AccessPermit>>,
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(permit) = permit {
        let cancellation = request
            .extensions()
            .get::<std::sync::Arc<super::ReadyServices>>()
            .map(|services| services.cancellation.clone());
        let body = std::mem::replace(request.body_mut(), Body::empty());
        *request.body_mut() = guarded_body(body, permit, cancellation, false);
    }
    next.run(request).await
}

/// 下载与 SSE 随访问结束关闭；登录、退出和命令响应不挂此层，保证终态结果可以送达。
pub(super) async fn guard_response_body(
    Extension(permit): Extension<Option<AccessPermit>>,
    request: Request,
    next: Next,
) -> Response {
    let cancellation = request
        .extensions()
        .get::<std::sync::Arc<super::ReadyServices>>()
        .map(|services| services.cancellation.clone());
    let mut response = next.run(request).await;
    if let Some(permit) = permit {
        let body = std::mem::replace(response.body_mut(), Body::empty());
        *response.body_mut() = guarded_body(body, permit, cancellation, true);
    }
    response
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

/// 验证真实网络来源并解析凭据，供 HTTP 与终端首帧共用。
/// 返回 None 表示没有有效登录，由具体入口决定拒绝还是允许重新登录／首帧认证。
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
        && (origin.is_some() || !matches!(*request.method(), Method::GET | Method::HEAD))
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

/// 上传被取消必须返回读取错误，不能把半个文件当成正常 EOF；响应流则正常结束。
/// biased 保证取消与下一块数据同时就绪时，优先停止传输。
fn guarded_body(
    body: Body,
    permit: AccessPermit,
    cancellation: Option<tokio_util::sync::CancellationToken>,
    graceful_end: bool,
) -> Body {
    Body::from_stream(async_stream::stream! {
        let mut chunks = body.into_data_stream();
        loop {
            let next = tokio::select! {
                biased;
                () = async {
                    if let Some(cancel) = &cancellation {
                        cancel.cancelled().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => if graceful_end {
                    None
                } else {
                    Some(Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied, "user domain ended",
                    )))
                },
                () = permit.ended() => if graceful_end {
                    None
                } else {
                    Some(Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied, "login ended",
                    )))
                },
                next = chunks.next() => next.map(|chunk| {
                    chunk.map_err(|_| std::io::Error::other("transport body failed"))
                }),
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
                    "x-ez-login-context",
                    "x-ez-host-bootstrap",
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
                "authorization,content-type,x-ez-client-version,x-ez-min-compatible-version,x-ez-login-context,x-ez-host-bootstrap",
            ),
        );
    }
    response
}

fn is_local_request(peer: std::net::SocketAddr, server_name: &str) -> bool {
    peer.ip().is_loopback() && matches!(server_name, "127.0.0.1" | "localhost")
}

/// 原生路径同时需要用户身份与真实本机管理证明；后者只证明调用位置，不选择用户域。
fn has_local_proof(state: &HttpState, request: &Request) -> bool {
    let Some(token) = request
        .headers()
        .get("x-ez-host-bootstrap")
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let mut proof = Request::new(Body::empty());
    *proof.headers_mut() = request.headers().clone();
    let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) else {
        return false;
    };
    proof.headers_mut().insert(AUTHORIZATION, value);
    if let Some(peer) = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
    {
        proof.extensions_mut().insert(*peer);
    }
    authorize_request(state, &proof)
        .is_ok_and(|(_, permit)| permit.is_some_and(|permit| permit.native))
}

/// fetch 通过头传递账号上下文；浏览器直接加载的 GET 资源可通过 query 传递。
/// 该值只用于发现旧页面，不是凭据，不能据此选择或切换用户。
fn expected_login_context(request: &Request) -> Option<String> {
    if let Some(value) = request.headers().get("x-ez-login-context") {
        return value.to_str().ok().map(str::to_owned);
    }
    if request.method() != Method::GET {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Query {
        login_context: Option<String>,
    }
    axum::extract::Query::<Query>::try_from_uri(request.uri())
        .ok()
        .and_then(|query| query.0.login_context)
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
