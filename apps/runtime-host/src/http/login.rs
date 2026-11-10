//! 普通登录与快捷登录复用一个入口；浏览器只获得 HttpOnly Cookie。

use assistant_protocol::{
    ClientCompatibility, HostIdentityKind, HostLoginRequest, HostLoginResult, HostLogoutResult,
    HostPasswordRequest,
};
use axum::{
    Extension, Json,
    extract::{State, rejection::JsonRejection},
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
    request: Result<Json<HostLoginRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = request else {
        return access_error(AccessError::Invalid("登录请求格式无效。"));
    };
    let credentials = &state.access.credentials;
    let result = match request {
        HostLoginRequest::Enterprise {
            username,
            password,
            native,
        } => {
            if !native && let Err(error) = require_same_origin(&state, &headers) {
                return error.into_response();
            }
            let Some(client) = state.access.center() else {
                return access_error(AccessError::Invalid("当前 Host 不是企业模式。"));
            };
            let selection = match credentials
                .begin_enterprise_login(&client, username, password, native)
                .await
            {
                Ok(selection) => selection,
                Err(error) => return access_error(error),
            };
            let mut response = Json(HostLoginResult {
                token: None,
                expires_at_ms: 0,
                instance_id: state.instance_id.to_string(),
                mode: state.access.mode(),
                kind: HostIdentityKind::User,
                identity: None,
                login_context: None,
                organization_selection: Some(selection),
            })
            .into_response();
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return response;
        }
        HostLoginRequest::EnterpriseSelect {
            selection_token,
            organization_id,
        } => {
            match credentials.selection_native(&selection_token) {
                Ok(false) => {
                    if let Err(error) = require_same_origin(&state, &headers) {
                        return error.into_response();
                    }
                }
                Ok(true) => {}
                Err(error) => return access_error(error),
            }
            credentials
                .select_enterprise_organization(
                    &state.access,
                    selection_token,
                    organization_id,
                    compatibility.clone(),
                    &state.shutdown,
                )
                .await
        }
        HostLoginRequest::Password { password, native } => {
            if state.access.center().is_some() {
                return access_error(AccessError::Unauthorized);
            }
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
            if state.access.center().is_some() {
                let Some(session) = credentials.authenticate(token.expose()) else {
                    return access_error(AccessError::Unauthorized);
                };
                let mut source = AccessPermit::new(false, Some(session), state.connections.clone());
                if let Err(error) = credentials.verify_enterprise(&mut source).await {
                    return access_error(error);
                }
            }
            credentials
                .exchange(token.expose(), compatibility.clone())
                .map(|(token, session)| (token, session, false))
        }
        HostLoginRequest::Desktop => {
            let Some(mut source) = permit.clone() else {
                return access_error(AccessError::Unauthorized);
            };
            if state.access.center().is_some()
                && let Err(error) = credentials.verify_enterprise(&mut source).await
            {
                return access_error(error);
            }
            credentials
                .issue(&source, compatibility.clone())
                .map(|(token, session)| (token, session, true))
        }
    };
    let (token, session, native) = match result {
        Ok(result) => result,
        Err(error) => return access_error(error),
    };
    if !native
        && !headers.contains_key(axum::http::header::AUTHORIZATION)
        && let Some(previous) = &permit
    {
        // Cookie 切换只结束被替换的浏览器登录，不结束该用户统一企业状态。
        previous.logout();
    }
    let permit = AccessPermit::new(false, Some(session.clone()), state.connections.clone());
    if permit.user_key().is_some()
        && let Some(domains) = &state.domains
    {
        domains.warm_up(&permit);
    }
    let mut projection = project_session(&state, &permit);
    projection.token = native.then(|| token.clone());
    let mut response = Json(projection).into_response();
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
    let mut response = if state.access.center().is_some() {
        if permit.user_key().is_none() {
            return access_error(AccessError::Unauthorized);
        }
        let confirmed = state.access.credentials.enterprise_logout(&permit).await;
        Json(HostLogoutResult {
            local_ended: true,
            center_revocation_confirmed: confirmed,
        })
        .into_response()
    } else {
        state.access.credentials.end_personal_login();
        StatusCode::NO_CONTENT.into_response()
    };
    if let Some(domains) = &state.domains
        && let Err(error) = domains.finish_logout(permit.user_key()).await
    {
        response = super::error::HttpError::from_domain(error).into_response();
    }
    if state
        .terminals
        .finish_logout(permit.user_key())
        .await
        .is_err()
    {
        response = super::error::HttpError::unavailable().into_response();
    }
    set_cookie(&mut response, state.secure, &headers, "", 0);
    response
}

pub(super) async fn session(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
) -> Json<HostLoginResult> {
    Json(project_session(&state, &permit))
}

fn project_session(state: &HttpState, permit: &AccessPermit) -> HostLoginResult {
    HostLoginResult {
        token: None,
        expires_at_ms: permit.expires_at_ms(),
        instance_id: state.instance_id.to_string(),
        mode: state.access.mode(),
        kind: if permit.native {
            HostIdentityKind::Bootstrap
        } else {
            HostIdentityKind::User
        },
        identity: state.access.credentials.identity(permit),
        login_context: permit.login_context(),
        organization_selection: None,
    }
}

pub(super) async fn password(
    State(state): State<HttpState>,
    Extension(permit): Extension<AccessPermit>,
    request: Result<Json<HostPasswordRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(request)) = request else {
        return access_error(AccessError::Invalid("改密请求格式无效。"));
    };
    match state
        .access
        .credentials
        .enterprise_password(&permit, &request)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => access_error(error),
    }
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

pub(super) fn access_error(error: AccessError) -> Response {
    use crate::access::enterprise::CenterError;
    let status = match error {
        AccessError::Center(
            CenterError::InvalidCredentials | CenterError::AuthenticationFailed,
        ) => StatusCode::UNAUTHORIZED,
        AccessError::Center(
            CenterError::IdentityMismatch
            | CenterError::ProtocolIncompatible
            | CenterError::ModelCapabilityMissing
            | CenterError::StateChanged,
        ) => StatusCode::CONFLICT,
        AccessError::Center(CenterError::Unavailable) => StatusCode::SERVICE_UNAVAILABLE,
        AccessError::Center(CenterError::InvalidRequest) => StatusCode::BAD_REQUEST,
        AccessError::Center(CenterError::Busy) => StatusCode::TOO_MANY_REQUESTS,
        AccessError::NetworkDenied => StatusCode::FORBIDDEN,
        AccessError::NetworkEnded => StatusCode::CONFLICT,
        AccessError::Unauthorized => StatusCode::UNAUTHORIZED,
        AccessError::Busy => StatusCode::TOO_MANY_REQUESTS,
        AccessError::Conflict => StatusCode::CONFLICT,
        AccessError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        AccessError::Invalid(_) | AccessError::PortInUse(_) => StatusCode::BAD_REQUEST,
    };
    let code = match &error {
        AccessError::Unauthorized | AccessError::Center(CenterError::InvalidCredentials) => {
            "authentication_required"
        }
        AccessError::Center(CenterError::Unavailable) => "center_unavailable",
        AccessError::Center(CenterError::IdentityMismatch) => "center_identity_mismatch",
        AccessError::Center(CenterError::ProtocolIncompatible) => "center_protocol_incompatible",
        AccessError::Center(CenterError::AuthenticationFailed) => "authentication_failed",
        AccessError::Center(CenterError::StateChanged) | AccessError::Conflict => {
            "configuration_conflict"
        }
        AccessError::Busy | AccessError::Center(CenterError::Busy) => "busy",
        AccessError::Unavailable => "configuration_unavailable",
        AccessError::NetworkDenied => "device_access_denied",
        AccessError::NetworkEnded => "device_access_ended",
        _ => "invalid_request",
    };
    (
        status,
        Json(serde_json::json!({"error": {"code": code, "message": error.to_string()}})),
    )
        .into_response()
}

pub(super) const ORGANIZATION_SWITCH_PATH: &str = "/auth/organization-switch";

pub(super) async fn organization_switch(
    State(state): State<HttpState>,
    Extension(permit): Extension<Option<AccessPermit>>,
    headers: HeaderMap,
    request: Result<Json<assistant_protocol::HostOrganizationSwitchRequest>, JsonRejection>,
) -> Response {
    use crate::{access::digest, user_domain::SwitchOutcome};
    use assistant_protocol::{
        HostOrganizationSwitchRequest as Request, HostOrganizationSwitchResult as Result,
    };
    use axum::http::header::AUTHORIZATION;

    let Ok(Json(request)) = request else {
        return access_error(AccessError::Invalid("组织切换请求格式无效。"));
    };
    let native = headers.contains_key(AUTHORIZATION);
    if !native && let Err(error) = require_same_origin(&state, &headers) {
        return error.into_response();
    }
    let Some(domains) = &state.domains else {
        return access_error(AccessError::Unavailable);
    };

    // 只有状态查询可携带刚被替换的会话。其他操作仍需有效登录及页面上下文。
    let outcome = if let Request::Status { operation_id } = request {
        let token = if native {
            headers
                .get(AUTHORIZATION)
                .and_then(|header| header.to_str().ok())
                .and_then(|header| header.strip_prefix("Bearer "))
        } else {
            super::auth::session_cookie(&headers, &cookie_name(state.secure, &headers))
                .ok()
                .flatten()
        };
        let Some(token) = token else {
            return access_error(AccessError::Unauthorized);
        };
        match domains.switch_status(&operation_id, digest(token)) {
            Some(outcome) => outcome,
            None if permit.is_some() => SwitchOutcome::Changed,
            None => return access_error(AccessError::Unauthorized),
        }
    } else {
        let Some(permit) = permit.filter(|permit| permit.user_key().is_some()) else {
            return access_error(AccessError::Unauthorized);
        };
        if !native
            && headers
                .get("x-ez-login-context")
                .and_then(|value| value.to_str().ok())
                != permit.login_context().as_deref()
        {
            return access_error(AccessError::Conflict);
        }
        match request {
            Request::Options | Request::Preview { .. } => {
                let (_, organizations) =
                    match state.access.credentials.switch_organizations(&permit).await {
                        Ok(value) => value,
                        Err(error) => return access_error(error),
                    };
                if let Request::Preview { organization_id } = request {
                    let Some(organization) = organizations
                        .into_iter()
                        .find(|org| org.id == organization_id)
                    else {
                        return access_error(AccessError::Conflict);
                    };
                    return match domains
                        .preview_switch(&permit, organization, &state.terminals)
                        .await
                    {
                        Ok(result) => switch_response(result),
                        Err(error) => super::error::HttpError::from_domain(error).into_response(),
                    };
                }
                return switch_response(Result::Options { organizations });
            }
            Request::Confirm { operation_id } => {
                match domains.confirm_switch(state.clone(), permit, &operation_id) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        return super::error::HttpError::from_domain(error).into_response();
                    }
                }
            }
            Request::Resume { operation_id } => {
                return match domains.resume_after_switch(&permit, &operation_id).await {
                    Ok(()) => switch_response(Result::Resumed),
                    Err(error) => super::error::HttpError::from_domain(error).into_response(),
                };
            }
            Request::Status { .. } => unreachable!("status is handled separately"),
        }
    };
    let result = match outcome {
        SwitchOutcome::Changed => Result::Changed,
        SwitchOutcome::Stopping => Result::Stopping,
        SwitchOutcome::Exchanging => Result::Exchanging,
        SwitchOutcome::StopFailed => Result::StopFailed,
        SwitchOutcome::ExchangeFailed => Result::ExchangeFailed,
        SwitchOutcome::Reauthenticate => Result::Reauthenticate,
        SwitchOutcome::Switched(token, session) => {
            if !session.valid() {
                return switch_response(Result::Reauthenticate);
            }
            let permit = AccessPermit::new(false, Some(session), state.connections.clone());
            let mut login = project_session(&state, &permit);
            if login.identity.is_none() {
                return switch_response(Result::Reauthenticate);
            }
            let expires_at_ms = login.expires_at_ms;
            login.token = native.then(|| token.clone());
            let mut response = switch_response(Result::Switched { login });
            if !native {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|time| time.as_millis() as u64)
                    .unwrap_or(expires_at_ms);
                set_cookie(
                    &mut response,
                    state.secure,
                    &headers,
                    token.expose(),
                    expires_at_ms.saturating_sub(now_ms) / 1000,
                );
            }
            return response;
        }
    };
    switch_response(result)
}

fn switch_response(result: assistant_protocol::HostOrganizationSwitchResult) -> Response {
    let mut response = Json(result).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
