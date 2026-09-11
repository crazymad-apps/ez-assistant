//! 普通登录与快捷登录复用一个入口；浏览器只获得 HttpOnly Cookie。

use assistant_protocol::{ClientCompatibility, HostLoginRequest, HostLoginResult};
use axum::{
    Extension, Json,
    extract::State,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, SET_COOKIE},
    },
    response::{IntoResponse, Response},
};

use super::{
    HttpState,
    auth::{cookie_name, require_same_origin},
};
use crate::access::{AccessError, AccessPermit};

pub(super) async fn login(
    State(state): State<HttpState>,
    Extension(permit): Extension<Option<AccessPermit>>,
    headers: HeaderMap,
    Extension(compatibility): Extension<ClientCompatibility>,
    Json(request): Json<HostLoginRequest>,
) -> Response {
    let credentials = &state.access.credentials;
    let result = match request {
        HostLoginRequest::Password { password, native } => {
            // native 仅选择 Token 响应，不代表本机管理身份。
            if !native && let Err(error) = require_same_origin(&state, &headers) {
                return error.into_response();
            }
            credentials
                .login(password, compatibility.clone())
                .await
                .map(|(token, session)| (token, session, native))
        }
        HostLoginRequest::Token { token } => {
            if let Err(error) = require_same_origin(&state, &headers) {
                return error.into_response();
            }
            credentials
                .exchange(token.expose(), compatibility.clone())
                .map(|(token, session)| (token, session, false))
        }
        HostLoginRequest::Desktop => permit
            .as_ref()
            .ok_or(AccessError::Unauthorized)
            .and_then(|permit| credentials.issue(permit, compatibility.clone()))
            .map(|(token, session)| (token, session, true)),
    };
    let (token, session, native) = match result {
        Ok(result) => result,
        Err(error) => return access_error(error),
    };
    let mut response = Json(HostLoginResult {
        token: if native { Some(token.clone()) } else { None },
        expires_at_ms: session.expires_at_ms,
        instance_id: state.instance_id.to_string(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if !native {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_millis() as u64)
            .unwrap_or(session.expires_at_ms);
        let remaining = session.expires_at_ms.saturating_sub(now_ms) / 1000;
        set_cookie(
            &mut response,
            state.secure,
            &headers,
            token.expose(),
            remaining,
        );
    }
    response
}

pub(super) async fn logout(
    headers: HeaderMap,
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
) -> Response {
    permit.logout();
    let mut response = StatusCode::NO_CONTENT.into_response();
    set_cookie(&mut response, state.secure, &headers, "", 0);
    response
}

pub(super) async fn session(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
) -> Json<HostLoginResult> {
    Json(HostLoginResult {
        token: None,
        expires_at_ms: permit.expires_at_ms(),
        instance_id: state.instance_id.to_string(),
    })
}

fn set_cookie(
    response: &mut Response,
    secure: bool,
    headers: &HeaderMap,
    token: &str,
    lifetime: u64,
) {
    let secure_attribute = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={lifetime}{secure_attribute}",
        cookie_name(secure, headers)
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(SET_COOKIE, value);
    }
}

fn access_error(error: AccessError) -> Response {
    let status = match error {
        AccessError::Unauthorized => StatusCode::UNAUTHORIZED,
        AccessError::Busy => StatusCode::TOO_MANY_REQUESTS,
        AccessError::Conflict => StatusCode::CONFLICT,
        AccessError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        AccessError::Invalid(_) | AccessError::PortInUse(_) => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(serde_json::json!({"error": {"message": error.to_string()}})),
    )
        .into_response()
}
